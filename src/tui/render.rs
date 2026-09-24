//! Drawing: the `Theme` palette, the free functions that turn panel state
//! into styled `Line`s, and the `App` methods that lay out frames. Pulls
//! formatted text from `format.rs` and reads `App` state from `state.rs`.

use ratatui::layout::{Alignment, Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};
use serde_json::Value;

use crate::api::AttachmentMetadata;
use crate::attachment::human_size;

use super::actions::{
    IncidentActionForm, IncidentActionKind, IncidentActionMenu, PreparedIncidentAction,
};
use super::format::{
    display_field, display_field_value, field_label, field_rank, field_weight, percentage_field,
    raw_field_string, record_title, safe_text, tail_text, truncate_text, truthy_field,
    visible_columns, wrap_text_exact,
};
use super::state::{App, IncidentTab, NoticeKind, Overlay, PanelState, panel_count_label};

pub(super) fn scroll_extent(lines: &[Line<'_>], width: usize, height: usize) -> u16 {
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

pub(super) fn activity_lines(state: &PanelState<Value>, theme: Theme) -> Vec<Line<'static>> {
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

pub(super) fn attachment_lines(
    state: &PanelState<AttachmentMetadata>,
    theme: Theme,
) -> Vec<Line<'static>> {
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

pub(super) fn sla_lines(state: &PanelState<Value>, theme: Theme) -> Vec<Line<'static>> {
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

#[derive(Clone, Copy, Debug)]
pub(super) struct Theme {
    color: bool,
}

impl Theme {
    pub(super) fn new(color: bool) -> Self {
        Self { color }
    }

    pub(super) fn canvas(self) -> Style {
        self.style(Color::Rgb(211, 220, 218), Color::Rgb(8, 14, 18))
    }

    pub(super) fn body(self) -> Style {
        self.style(Color::Rgb(211, 220, 218), Color::Reset)
    }

    pub(super) fn muted(self) -> Style {
        self.style(Color::Rgb(123, 145, 145), Color::Reset)
    }

    pub(super) fn brand(self) -> Style {
        self.style(Color::Rgb(4, 24, 27), Color::Rgb(101, 240, 202))
            .add_modifier(Modifier::BOLD)
    }

    pub(super) fn title(self) -> Style {
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

    pub(super) fn key(self) -> Style {
        self.style(Color::Rgb(246, 183, 82), Color::Reset)
            .add_modifier(Modifier::BOLD)
    }

    pub(super) fn rule(self) -> Style {
        self.style(Color::Rgb(53, 74, 77), Color::Reset)
    }

    pub(super) fn active_rule(self) -> Style {
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

pub(super) fn render_input(
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

pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
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

pub(super) fn state_panel_area(area: Rect) -> Rect {
    if area.height < 12 {
        inset(area, 1, 0)
    } else {
        inset(area, 2, 2)
    }
}

impl App {
    pub(super) fn render(&mut self, frame: &mut ratatui::Frame<'_>) {
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

        let overlay = self.overlay.clone();
        let action_returns_to_detail = match &overlay {
            Overlay::IncidentActions(menu) => menu.target.return_tab.is_some(),
            Overlay::IncidentActionForm(form) => form.target.return_tab.is_some(),
            Overlay::IncidentActionReview(prepared) => prepared.form.target.return_tab.is_some(),
            _ => false,
        };
        if action_returns_to_detail {
            self.render_detail_sheet(frame, frame.area(), theme, true);
        }
        match overlay {
            Overlay::Help { .. } => self.render_help(frame, theme),
            Overlay::TableInput(buffer) => {
                render_input(frame, theme, "GO TO TABLE", "Table name", &buffer)
            }
            Overlay::QueryInput(buffer) => render_input(
                frame,
                theme,
                "QUERY SERVICENOW",
                "ServiceNow encoded query; blank clears",
                &buffer,
            ),
            Overlay::SearchInput(buffer) => render_input(
                frame,
                theme,
                "SEARCH THIS PAGE",
                "Matches loaded display values; blank clears",
                &buffer,
            ),
            Overlay::IncidentActions(menu) => self.render_incident_actions(frame, theme, &menu),
            Overlay::IncidentActionForm(form) => {
                self.render_incident_action_form(frame, theme, &form)
            }
            Overlay::IncidentActionReview(prepared) => {
                self.render_incident_action_review(frame, theme, &prepared)
            }
            Overlay::Detail => self.render_detail_sheet(frame, frame.area(), theme, true),
            Overlay::None => {}
        }
    }

    fn render_header(&self, frame: &mut ratatui::Frame<'_>, area: Rect, theme: Theme) {
        let title = Line::from(vec![
            Span::styled(" SERVICENOW ", theme.brand()),
            Span::styled(" OPERATIONS LEDGER", theme.title()),
        ]);
        let safety = if self.read_only {
            "  ·  READ ONLY"
        } else {
            ""
        };
        let location = if area.width >= 80 {
            format!(
                "{}  ·  {}  /  {}  ·  page {}{}",
                safe_text(&self.profile),
                safe_text(&self.instance),
                self.table,
                self.offset / self.page_size + 1,
                safety
            )
        } else {
            format!(
                "{}  /  {}  ·  page {}{}",
                safe_text(&self.profile),
                self.table,
                self.offset / self.page_size + 1,
                safety
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

        let matching_indices = self.matching_record_indices().collect::<Vec<_>>();
        if matching_indices.is_empty() {
            let search = safe_text(self.search.as_deref().unwrap_or_default());
            let recovery = if self.has_next_page {
                Line::from(vec![
                    Span::styled("s", theme.key()),
                    Span::styled(" change or clear search   ", theme.muted()),
                    Span::styled("n", theme.key()),
                    Span::styled(" search next page", theme.muted()),
                ])
            } else {
                Line::from(vec![
                    Span::styled("s", theme.key()),
                    Span::styled(" change or clear search   ", theme.muted()),
                    Span::styled("/", theme.key()),
                    Span::styled(" broaden query", theme.muted()),
                ])
            };
            let empty = Paragraph::new(vec![
                Line::styled("NO LOCAL MATCHES", theme.title()),
                Line::styled(
                    format!("No records on this loaded page contain '{}'.", search),
                    theme.muted(),
                ),
                recovery,
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" LOCAL SEARCH ")
                    .borders(Borders::ALL)
                    .border_style(theme.rule()),
            );
            frame.render_widget(empty, area);
            return;
        }

        let visible_columns = visible_columns(&self.columns, area.width);
        let rows = matching_indices.iter().map(|index| {
            let record = &self.records[*index];
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
        let title = if self.search.is_some() {
            format!(
                " RECORD INDEX  ·  {} OF {} LOCAL MATCH{} ",
                matching_indices.len(),
                self.records.len(),
                if matching_indices.len() == 1 {
                    ""
                } else {
                    "ES"
                }
            )
        } else {
            format!(
                " RECORD INDEX  {}–{} ",
                self.offset + 1,
                self.offset + self.records.len()
            )
        };
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
            .title(format!(
                " INCIDENT WORKSPACE  {title}  ·  A ACTIONS  ·  ESC BACK "
            ))
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

    pub(super) fn incident_tab_count(&self, tab: IncidentTab) -> Option<String> {
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

    pub(super) fn overview_lines<'a>(&self, record: &'a Value, theme: Theme) -> Vec<Line<'a>> {
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
        let context = self.search.as_deref().map_or_else(
            || format!("QUERY  {query}"),
            |search| format!("SEARCH  {}  ·  QUERY  {query}", safe_text(search)),
        );
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
            let mut spans = vec![
                Span::styled("↑↓", theme.key()),
                Span::styled(" move  ", theme.muted()),
                Span::styled("enter", theme.key()),
                Span::styled(" inspect  ", theme.muted()),
            ];
            if self.table == "incident" && self.selected_record().is_some() {
                spans.push(Span::styled("a", theme.key()));
                spans.push(Span::styled(" actions  ", theme.muted()));
            }
            if area.width >= 80 || self.table != "incident" {
                spans.extend([
                    Span::styled("s", theme.key()),
                    Span::styled(" search  ", theme.muted()),
                ]);
            }
            if area.width >= 80 {
                spans.extend([
                    Span::styled("/", theme.key()),
                    Span::styled(" query  ", theme.muted()),
                    Span::styled("t", theme.key()),
                    Span::styled(" table  ", theme.muted()),
                ]);
            }
            if area.width >= 112 {
                spans.push(Span::styled("p", previous_key));
                spans.push(Span::styled(
                    if self.offset > 0 {
                        " prev  "
                    } else {
                        " start  "
                    },
                    theme.muted(),
                ));
                spans.push(Span::styled("n", next_key));
                spans.push(Span::styled(
                    if self.has_next_page {
                        " next  "
                    } else {
                        " end  "
                    },
                    theme.muted(),
                ));
            } else if area.width >= 80 {
                spans.extend([
                    Span::styled("p/n", theme.key()),
                    Span::styled(" page  ", theme.muted()),
                ]);
            }
            spans.extend([
                Span::styled("?", theme.key()),
                Span::styled(" help  ", theme.muted()),
                Span::styled("q", theme.key()),
                Span::styled(" quit", theme.muted()),
            ]);
            Line::from(spans)
        };
        let status = if area.width >= 90 {
            Line::from(vec![
                Span::styled(format!(" {notice} "), notice_style),
                Span::styled(
                    format!("  {}", truncate_text(&context, notice_limit)),
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
            ("a", "Open guarded actions for the selected incident"),
            ("tab / shift-tab", "Move through incident detail views"),
            ("1 / 2 / 3 / 4", "Open Overview, Activity, Files, or SLAs"),
            ("t", "Browse another table"),
            ("s", "Search display values on the loaded page"),
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
            if self.read_only {
                "This profile is read-only; incident actions cannot write."
            } else {
                "Every incident write is previewed and explicitly confirmed."
            },
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

    fn render_incident_actions(
        &self,
        frame: &mut ratatui::Frame<'_>,
        theme: Theme,
        menu: &IncidentActionMenu,
    ) {
        let width = frame.area().width.saturating_sub(4).min(78);
        let area = centered_rect(width, 15, frame.area());
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(format!(
                " INCIDENT ACTIONS  {}  ·  ESC CANCEL ",
                safe_text(&menu.target.title)
            ))
            .borders(Borders::ALL)
            .border_style(theme.active_rule())
            .style(theme.canvas());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let compact = inner.height < 13;
        let status = if self.read_only {
            format!(
                "Profile '{}' is read-only. Actions are visible but unavailable.",
                safe_text(&self.profile)
            )
        } else {
            "Choose one focused update. You will review it before anything changes.".into()
        };
        let status_style = if self.read_only {
            theme.error()
        } else {
            theme.muted()
        };
        let mut lines = wrap_text_exact(&status, usize::from(inner.width.max(1)))
            .into_iter()
            .take(if compact { 2 } else { usize::MAX })
            .map(|line| Line::styled(line, status_style))
            .collect::<Vec<_>>();
        if !compact {
            lines.push(Line::raw(""));
        }
        for (index, kind) in IncidentActionKind::ALL.into_iter().enumerate() {
            let selected = index == menu.selected;
            lines.push(Line::from(vec![Span::styled(
                format!(
                    "{}{}  {}",
                    if selected { "▌" } else { " " },
                    index + 1,
                    kind.label()
                ),
                if selected {
                    theme.active_tab()
                } else {
                    theme.field()
                },
            )]));
            if !compact {
                lines.push(Line::styled(
                    format!("    {}", kind.description()),
                    theme.muted(),
                ));
            }
        }
        if !compact {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled("↑↓", theme.key()),
            Span::styled(" choose  ", theme.muted()),
            Span::styled("1–3/enter", theme.key()),
            Span::styled(
                if compact { " open  " } else { " continue  " },
                theme.muted(),
            ),
            Span::styled("esc", theme.key()),
            Span::styled(" cancel", theme.muted()),
        ]));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_incident_action_form(
        &self,
        frame: &mut ratatui::Frame<'_>,
        theme: Theme,
        form: &IncidentActionForm,
    ) {
        let width = frame.area().width.saturating_sub(4).min(86);
        let height = (7 + form.fields.len() as u16 * 3).min(frame.area().height);
        let area = centered_rect(width, height, frame.area());
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(format!(
                " {}  {}  ·  ESC ACTIONS ",
                form.kind.label(),
                safe_text(&form.target.title)
            ))
            .borders(Borders::ALL)
            .border_style(theme.active_rule())
            .style(theme.canvas());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let compact = inner.height < 12;
        let notice_style = match self.notice.kind {
            NoticeKind::Quiet => theme.muted(),
            NoticeKind::Success => theme.success(),
            NoticeKind::Error => theme.error(),
        };
        let notice_lines = wrap_text_exact(&self.notice.text, usize::from(inner.width.max(1)))
            .into_iter()
            .take(2)
            .collect::<Vec<_>>();
        let mut lines = Vec::new();
        if !compact {
            lines.push(Line::styled(
                "Enter advances fields; the final Enter prepares a review.",
                theme.muted(),
            ));
        }
        lines.extend(
            notice_lines
                .iter()
                .cloned()
                .map(|line| Line::styled(line, notice_style)),
        );
        if !compact {
            lines.push(Line::raw(""));
        }
        for (index, field) in form.fields.iter().enumerate() {
            let focused = index == form.focused;
            let mut label = vec![
                Span::styled(
                    if focused { "▌" } else { " " },
                    if focused {
                        theme.active_tab()
                    } else {
                        theme.muted()
                    },
                ),
                Span::styled(
                    format!("{}  ", field.label),
                    if focused {
                        theme.field()
                    } else {
                        theme.muted()
                    },
                ),
            ];
            if !compact {
                label.push(Span::styled(field.hint, theme.muted()));
            }
            lines.push(Line::from(label));
            let visible = safe_text(&field.value.replace('\n', " ↵ "));
            let input_width = usize::from(inner.width.saturating_sub(4).max(1));
            let visible = tail_text(&visible, input_width);
            lines.push(Line::from(vec![
                Span::styled("  › ", theme.key()),
                Span::styled(
                    visible,
                    if focused {
                        theme.body().add_modifier(Modifier::BOLD)
                    } else {
                        theme.body()
                    },
                ),
            ]));
            if !compact {
                lines.push(Line::raw(""));
            }
        }
        lines.push(Line::from(vec![
            Span::styled(
                if compact {
                    "tab/↑↓"
                } else {
                    "tab / ↑↓"
                },
                theme.key(),
            ),
            Span::styled(" fields  ", theme.muted()),
            Span::styled("enter", theme.key()),
            Span::styled(
                if compact { " review  " } else { " continue  " },
                theme.muted(),
            ),
            Span::styled("esc", theme.key()),
            Span::styled(" actions", theme.muted()),
        ]));
        frame.render_widget(Paragraph::new(lines), inner);

        let field = &form.fields[form.focused];
        let input_width = usize::from(inner.width.saturating_sub(4).max(1));
        let visible_length = tail_text(&safe_text(&field.value.replace('\n', " ↵ ")), input_width)
            .chars()
            .count() as u16;
        let cursor_offset = if compact {
            notice_lines.len() as u16 + form.focused as u16 * 2 + 1
        } else {
            3 + notice_lines.len() as u16 + form.focused as u16 * 3
        };
        let cursor_y = inner
            .y
            .saturating_add(cursor_offset)
            .min(inner.bottom().saturating_sub(1));
        let cursor_x = inner
            .x
            .saturating_add(4)
            .saturating_add(visible_length)
            .min(inner.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    fn render_incident_action_review(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        theme: Theme,
        prepared: &PreparedIncidentAction,
    ) {
        let width = frame.area().width.saturating_sub(4).min(88);
        let height = (9 + prepared.preview.len() as u16 * 3).min(frame.area().height);
        let area = centered_rect(width, height, frame.area());
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(format!(
                " REVIEW WRITE  {}  ·  ESC EDIT ",
                safe_text(&prepared.form.target.title)
            ))
            .borders(Borders::ALL)
            .border_style(theme.active_rule())
            .style(theme.canvas());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);
        let notice_style = match self.notice.kind {
            NoticeKind::Quiet => theme.muted(),
            NoticeKind::Success => theme.success(),
            NoticeKind::Error => theme.error(),
        };
        let mut introduction_lines = vec![Line::styled(
            format!("{} · atomic ServiceNow PATCH", prepared.form.kind.label()),
            theme.title(),
        )];
        introduction_lines.extend(
            wrap_text_exact(&self.notice.text, usize::from(sections[0].width.max(1)))
                .into_iter()
                .take(2)
                .map(|line| Line::styled(line, notice_style)),
        );
        let introduction = Paragraph::new(introduction_lines);
        frame.render_widget(introduction, sections[0]);

        let preview_width = usize::from(sections[1].width.max(1));
        let mut lines = Vec::new();
        for (label, value) in &prepared.preview {
            lines.push(Line::styled(label.clone(), theme.field()));
            for value_line in wrap_text_exact(value, preview_width.saturating_sub(2).max(1)) {
                lines.push(Line::styled(format!("  {value_line}"), theme.body()));
            }
            lines.push(Line::raw(""));
        }
        let line_count = lines.len() as u16;
        let preview = Paragraph::new(lines).scroll((self.action_review_scroll, 0));
        self.action_review_viewport_height = sections[1].height.max(1);
        self.action_review_max_scroll =
            line_count.saturating_sub(self.action_review_viewport_height);
        self.action_review_scroll = self.action_review_scroll.min(self.action_review_max_scroll);
        frame.render_widget(preview, sections[1]);

        let footer = Line::from(vec![
            Span::styled("↑↓", theme.key()),
            Span::styled(" scroll  ", theme.muted()),
            Span::styled("enter", theme.key()),
            Span::styled(" apply  ", theme.muted()),
            Span::styled("esc", theme.key()),
            Span::styled(" edit  ", theme.muted()),
            Span::styled("q", theme.key()),
            Span::styled(" cancel", theme.muted()),
        ]);
        frame.render_widget(Paragraph::new(footer), sections[2]);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::style::Color;
    use serde_json::Map;

    use crate::api::AttachmentMetadata;

    use super::super::RELATED_VIEW_LIMIT;
    use super::super::actions::IncidentActionKind;
    use super::super::state::{Action, Notice, bounded_panel, panel_count, panel_truncated};
    use super::super::test_support::{action_form, app, rendered_text};
    use super::*;

    #[test]
    fn incident_action_review_keeps_confirmation_visible_and_scrolls_on_compact_terminals() {
        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.notice = Notice::quiet("Review the exact ServiceNow update before applying.");
        app.overlay = Overlay::IncidentActionReview(PreparedIncidentAction {
            form: action_form(IncidentActionKind::Resolve, None),
            body: Map::new(),
            preview: vec![
                ("STATE".into(), "Resolved".into()),
                ("RESOLUTION CODE".into(), "Solved (Permanently)".into()),
                (
                    "RESOLUTION NOTES".into(),
                    format!(
                        "{}§",
                        "Validated delivery with the requester and confirmed mail flow. ".repeat(8)
                    ),
                ),
            ],
        });

        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("REVIEW WRITE"));
        assert!(text.contains("Review the exact"));
        assert!(text.contains("enter apply"));
        assert!(app.action_review_max_scroll > 0);

        app.action_review_scroll = app.action_review_max_scroll;
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains('§'));
        assert!(text.contains("enter apply"));

        app.notice = Notice::error("Update rejected by a business rule.");
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("Update rejected"));
        assert!(text.contains("enter apply"));
    }

    #[test]
    fn compact_incident_action_chooser_and_form_keep_fields_and_controls_visible() {
        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.open_incident_actions(false);

        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("ADD WORK NOTE"));
        assert!(text.contains("ASSIGN"));
        assert!(text.contains("RESOLVE"));
        assert!(text.contains("1–3/enter open"));

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
            Action::None
        );
        app.notice = Notice::error("Resolution notes cannot be empty.");
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        assert!(text.contains("Resolution notes cannot be empty"));
        assert!(text.contains("RESOLUTION CODE"));
        assert!(text.contains("RESOLUTION NOTES"));
        assert!(text.contains("enter review"));
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
    fn local_search_empty_state_is_distinct_from_an_empty_service_now_view() {
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        app.apply_search("vpn");

        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = rendered_text(terminal.backend().buffer());
        if std::env::var_os("SERVICENOW_TUI_SNAPSHOT").is_some() {
            eprintln!("\n{text}");
        }
        assert!(text.contains("NO LOCAL MATCHES"));
        assert!(text.contains("No records on this loaded page contain 'vpn'"));
        assert!(!text.contains("THE LEDGER IS EMPTY"));
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
}
