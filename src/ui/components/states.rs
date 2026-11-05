use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{app::App, format::truncate};

/// Braille spinner, one frame per 100 ms tick.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Centered spinner with a message, for a page waiting on data.
pub fn loading(frame: &mut Frame, area: Rect, app: &App, message: &str) {
    let theme = app.theme;
    let line = Line::from(vec![
        Span::styled(
            SPINNER[app.spinner % SPINNER.len()],
            Style::new().fg(theme.accent),
        ),
        Span::styled(format!(" {message}"), Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        centered_rows(area, 1),
    );
}

/// Centered failure notice: what failed, why, and how to retry.
pub fn error(frame: &mut Frame, area: Rect, app: &App, title: &str, detail: &str) {
    let theme = app.theme;
    let detail_width = usize::from(area.width.saturating_sub(4));
    let lines = vec![
        Line::styled(
            format!("✗ {title}"),
            Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
        ),
        Line::styled(truncate(detail, detail_width), Style::new().fg(theme.muted)),
        Line::from(vec![
            Span::styled("按 ", Style::new().fg(theme.muted)),
            Span::styled(
                "r",
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" 重试", Style::new().fg(theme.muted)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        centered_rows(area, 3),
    );
}

/// The `rows` rows in the vertical middle of `area`.
fn centered_rows(area: Rect, rows: u16) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(rows),
        Constraint::Fill(1),
    ])
    .areas(area);
    middle
}
