use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;

/// Keys that work on every page, as `(key, what it does)`.
const GLOBAL_HINTS: [(&str, &str); 3] = [("1-7/Tab", "切换页面"), ("r", "刷新"), ("q", "退出")];

/// Divider plus one row of key hints: keys in accent, descriptions muted.
/// Needs two rows.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let divider = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(theme.muted));
    let hints_area = divider.inner(area);
    frame.render_widget(divider, area);

    let mut spans = Vec::with_capacity(GLOBAL_HINTS.len() * 3);
    for (index, (key, description)) in GLOBAL_HINTS.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            key,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {description}"),
            Style::new().fg(theme.muted),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), hints_area);
}
