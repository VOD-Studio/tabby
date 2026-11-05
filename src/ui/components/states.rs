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

/// The spinner frame to show right now.
pub fn spinner_frame(app: &App) -> &'static str {
    SPINNER[app.spinner % SPINNER.len()]
}

/// Centered spinner with a message, for a page waiting on data.
pub fn loading(frame: &mut Frame, area: Rect, app: &App, message: &str) {
    let theme = app.theme;
    let line = Line::from(vec![
        Span::styled(spinner_frame(app), Style::new().fg(theme.accent)),
        Span::styled(format!(" {message}"), Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        centered_rows(area, 1),
    );
}

/// Centered failure notice: what failed, why, and how to retry.
pub fn error(frame: &mut Frame, area: Rect, app: &App, title: &str, detail: Option<&str>) {
    let theme = app.theme;
    let detail_width = usize::from(area.width.saturating_sub(4));
    let mut lines = vec![Line::styled(
        format!("{} {title}", app.icons.cross),
        Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
    )];
    if let Some(detail) = detail {
        lines.push(Line::styled(
            truncate(detail, detail_width),
            Style::new().fg(theme.muted),
        ));
    }
    lines.push(Line::from(vec![
        Span::styled("按 ", Style::new().fg(theme.muted)),
        Span::styled(
            "r",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" 重试", Style::new().fg(theme.muted)),
    ]));
    let rows = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        centered_rows(area, rows),
    );
}

/// Centered notice for a list with nothing in it: why it is empty, and the
/// keys that can change that.
pub fn empty(frame: &mut Frame, area: Rect, app: &App, reason: &str, keys: &[(&str, &str)]) {
    let theme = app.theme;
    let mut hint = vec![];
    for (index, (key, description)) in keys.iter().enumerate() {
        if index > 0 {
            hint.push(Span::raw("   "));
        }
        hint.push(Span::styled(
            *key,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
        hint.push(Span::styled(
            format!(" {description}"),
            Style::new().fg(theme.muted),
        ));
    }
    let lines = vec![
        Line::styled(
            format!("{} {reason}", app.icons.empty),
            Style::new().fg(theme.fg),
        ),
        Line::from(hint),
    ];
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        centered_rows(area, 2),
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
