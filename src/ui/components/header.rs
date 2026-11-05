use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    api::models::CoreKind,
    app::{App, Connection},
};

/// One-line top bar: logo, kernel badge, mode pill; connection dot and profile
/// name on the right.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let bold = Modifier::BOLD;

    let mut left = vec![Span::styled(
        "◆ tabby",
        Style::new().fg(theme.accent).add_modifier(bold),
    )];
    if let Some(Ok(version)) = &app.version {
        let core = match version.kind() {
            CoreKind::Mihomo => "mihomo",
            CoreKind::Clash => "clash",
        };
        left.push(Span::styled(
            format!("  {core} {}", version.version),
            Style::new().fg(theme.muted),
        ));
    }
    if let Some(Ok(configs)) = &app.configs
        && !configs.mode.is_empty()
    {
        left.push(Span::raw("  "));
        // Reversed, so the pill is accent-filled in any palette.
        left.push(Span::styled(
            format!(" {} ", configs.mode.to_uppercase()),
            Style::new()
                .fg(theme.accent)
                .add_modifier(bold | Modifier::REVERSED),
        ));
    }

    let dot = match app.connection() {
        Connection::Connecting => theme.muted,
        Connection::Connected => theme.success,
        Connection::Disconnected => theme.error,
    };
    let right = Line::from(vec![
        Span::styled("●", Style::new().fg(dot)),
        Span::styled(
            format!(" {}", app.settings.profile),
            Style::new().fg(theme.fg),
        ),
    ]);

    let right_width = u16::try_from(right.width()).unwrap_or(u16::MAX);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(left)), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}
