use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding},
};

/// A `width` x `height` rectangle centered in `area`, shrunk to fit if needed.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Draws an overlay panel over `area`: wipes what is underneath, then a rounded
/// border in `border`, optionally titled, with `status` after the title in
/// its own style. Returns the inner area to draw content into.
pub fn panel(
    frame: &mut Frame,
    area: Rect,
    border: Color,
    title: Option<&str>,
    status: Option<Span<'static>>,
) -> Rect {
    let style = Style::new().fg(border);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(style)
        .padding(Padding::horizontal(1));
    if let Some(title) = title {
        let mut spans = vec![Span::styled(
            format!(" {title} "),
            style.add_modifier(Modifier::BOLD),
        )];
        spans.extend(status);
        block = block.title(Line::from(spans));
    }
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}
