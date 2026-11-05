use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::{app::App, app::Page, ui::page_title};

/// Numbered tab row (`1 概览  2 代理 …`) with a divider underneath. Needs two rows.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let divider = Block::new()
        .borders(Borders::BOTTOM)
        .border_style(Style::new().fg(theme.muted));
    let labels_area = divider.inner(area);
    frame.render_widget(divider, area);

    let mut spans = Vec::with_capacity(Page::ALL.len() * 2);
    for (index, page) in Page::ALL.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let label = format!("{} {}", index + 1, page_title(page));
        spans.push(if page == app.page {
            Span::styled(
                label,
                Style::new()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )
        } else {
            Span::styled(label, Style::new().fg(theme.muted))
        });
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), labels_area);
}
