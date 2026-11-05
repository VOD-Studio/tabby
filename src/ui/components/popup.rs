use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding},
};
use unicode_width::UnicodeWidthStr;

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
    split_wide_edges(frame.buffer_mut(), area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}

/// Blanks the wide characters that straddle the left or right edge of
/// `area`, so the panel's border is what the terminal shows there.
///
/// A wide character sits in the buffer as its symbol in the left cell and
/// an empty cell to its right, and the terminal draws both columns from the
/// left one. When that left cell is just outside `area`, clearing `area`
/// only empties the right one: the character stays and the border column
/// is not even sent, as the diff skips the cell a wide character covers.
/// The same character cut off on the right edge has its left cell inside,
/// which `Clear` wipes; it is blanked here all the same so that neither
/// edge depends on what runs after.
fn split_wide_edges(buffer: &mut Buffer, area: Rect) {
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    let outside_left = area.x.checked_sub(1).filter(|&x| x >= buffer.area.x);
    let rightmost = area.right() - 1;
    for y in area.top()..area.bottom() {
        for x in outside_left.into_iter().chain([rightmost]) {
            let cell = &mut buffer[(x, y)];
            if cell.symbol().width() > 1 {
                cell.set_symbol(" ");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};

    use super::*;

    #[test]
    fn borders_stay_whole_over_wide_characters() {
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        // `中` fills every row, two columns each from column 0: the panel's
        // left edge (1) is the right half of one, its right edge (8) the
        // left half of another.
        let area = Rect::new(1, 1, 8, 4);
        terminal
            .draw(|frame| {
                let line = "中".repeat(10);
                frame.render_widget(Paragraph::new(vec![Line::raw(line); 6]), frame.area());
                panel(frame, area, Color::Reset, None, None);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in area.top()..area.bottom() {
            let left = buffer[(area.left(), y)].symbol();
            let right = buffer[(area.right() - 1, y)].symbol();
            let (expected_left, expected_right) = match y {
                1 => ("╭", "╮"),
                4 => ("╰", "╯"),
                _ => ("│", "│"),
            };
            assert_eq!((left, right), (expected_left, expected_right), "row {y}");
            // The `中` whose right half the border took is gone, so the
            // terminal does not draw it over the border.
            assert_eq!(buffer[(area.left() - 1, y)].symbol(), " ", "row {y}");
        }
        // Rows outside the panel keep their characters.
        assert_eq!(buffer[(0, 0)].symbol(), "中");
        assert_eq!(buffer[(0, 5)].symbol(), "中");
    }
}
