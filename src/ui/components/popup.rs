use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, BorderType, Clear, Padding},
};

/// Draws an overlay panel over `area`: wipes what is underneath, then a rounded
/// border in `border`. Returns the inner area to draw content into.
pub fn panel(frame: &mut Frame, area: Rect, border: Color) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}
