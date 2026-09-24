// THESIS: A live operations ledger makes arbitrary ServiceNow records feel located, inspectable, and calm; it refuses the miniature-web-dashboard default.
// OWN-WORLD: Midnight ink, mint indexing, amber attention, hairline rules, dense rows, and one clearly punched active mark.
// STORY: See the active instance and query, scan records, unfold incidents into overview, activity, file, and SLA evidence, then return to the shell with context intact.
// FIRST VIEWPORT: Identity and location span the top; a dominant record ledger and persistent detail sheet share the field; incident inspection expands into a four-view workspace.
// FORM: Operations ledger, seventh grounded direction; seed 221c1ea6. Signature interaction: the selected ledger row unfolds into a progressively loaded incident workspace.
// FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

mod actions;
mod cancel;
mod format;
mod render;
mod state;
#[cfg(test)]
mod test_support;

use std::io::{self, IsTerminal};
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::api::{ApiError, ServiceNowClient, validate_table};
use crate::config::Config;

use format::safe_text;
use render::{Theme, centered_rect, state_panel_area};
use state::{Action, App, Notice};

pub use state::TuiExit;

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

    let mut app = App::new(&config.profile, &config.instance, config.read_only, options);
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
            Action::OpenIncidentActions { return_to_detail } => {
                app.open_incident_actions(return_to_detail);
            }
            Action::PrepareIncident(form) => {
                app.notice = Notice::quiet(format!("{} Esc cancels.", form.kind.progress()));
                terminal
                    .draw(|frame| app.render(frame))
                    .map_err(terminal_error)?;
                app.prepare_incident_action(client, form).await;
            }
            Action::ExecuteIncident(prepared) => {
                app.notice = Notice::quiet(format!(
                    "Applying {}… Esc cancels.",
                    prepared.form.kind.label()
                ));
                terminal
                    .draw(|frame| app.render(frame))
                    .map_err(terminal_error)?;
                app.execute_incident_action(client, config, prepared).await;
            }
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

fn terminal_error(error: io::Error) -> ApiError {
    ApiError::Other(format!("terminal error: {error}"))
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;

    use super::test_support::rendered_text;
    use super::*;

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
}
