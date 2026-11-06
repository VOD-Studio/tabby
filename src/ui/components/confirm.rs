use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::App,
    ui::{components::popup, text::wrap},
};

/// Widest the dialog gets, borders included.
const WIDTH: u16 = 48;

/// The confirm dialog over `area`: the question in [`App::pending`], centered
/// and wrapped, then the keys that answer it. Warning border: what it asks
/// about cannot be undone.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(pending) = &app.pending else {
        return;
    };
    let theme = app.theme;
    let width = WIDTH.min(area.width);
    // Borders and the panel's padding take two columns a side.
    let text_width = usize::from(width.saturating_sub(4)).max(1);
    let body = Span::styled(pending.body(), Style::new().fg(theme.fg));
    let key = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let muted = Style::new().fg(theme.muted);

    let mut lines = vec![Line::raw("")];
    lines.extend(
        wrap(vec![body], text_width)
            .into_iter()
            .map(|spans| Line::from(spans).centered()),
    );
    lines.push(Line::raw(""));
    lines.push(
        Line::from(vec![
            Span::styled("y", key),
            Span::styled(format!(" {}   ", pending.verb()), muted),
            Span::styled("n / Esc", key),
            Span::styled(" 取消", muted),
        ])
        .centered(),
    );

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = popup::modal(app, area, width, height);
    let inner = popup::panel(frame, rect, theme.warning, Some(pending.title()), None);
    frame.render_widget(Paragraph::new(lines), inner);
}
