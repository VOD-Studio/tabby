use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::{
    app::{App, Page},
    ui::{NARROW_WIDTH, hit::Target, page_title},
};

/// Numbered tab row (`1 概览  2 代理 …`, only `1  2 …` below
/// [`NARROW_WIDTH`]) with a divider underneath. Icon sets with page icons
/// put one before each number and title, but not before a bare number.
/// Needs two rows. A tab answers to clicks over its label and the blank
/// column on each side, so a bare number is not a one-cell target.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let divider = Block::new()
        .borders(Borders::BOTTOM)
        .border_style(Style::new().fg(theme.muted));
    let labels_area = divider.inner(area);
    frame.render_widget(divider, area);

    let numbers_only = area.width < NARROW_WIDTH;
    let mut spans = Vec::with_capacity(Page::ALL.len() * 2);
    let mut x = labels_area.x;
    for (index, page) in Page::ALL.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
            x = x.saturating_add(2);
        }
        let label = match app.icons.pages {
            _ if numbers_only => (index + 1).to_string(),
            Some(icons) => format!("{} {} {}", icons[index], index + 1, page_title(page)),
            None => format!("{} {}", index + 1, page_title(page)),
        };
        let width = u16::try_from(Span::raw(label.as_str()).width()).unwrap_or(u16::MAX);
        let left = if index > 0 { x - 1 } else { x };
        let right = x.saturating_add(width) + u16::from(index + 1 < Page::ALL.len());
        let hit = Rect::new(left, area.y, right - left, area.height).intersection(area);
        app.hits.add(hit, Target::Tab(page));
        x = x.saturating_add(width);
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
