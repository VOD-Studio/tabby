use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, BorderType, Clear, Padding, Paragraph},
};

use crate::{
    app::{App, ToastKind},
    format::truncate,
};

/// Widest toast, borders included.
const MAX_WIDTH: u16 = 50;

/// Toasts stacked down from the top-right corner, newest last.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let mut y = area.y;
    for toast in &app.toasts {
        let (icon, color) = match toast.kind {
            ToastKind::Success => ("✓", theme.success),
            ToastKind::Error => ("✗", theme.error),
        };
        let width = MAX_WIDTH.min(area.width);
        // Two border columns and one column of padding on each side.
        let text_width = usize::from(width.saturating_sub(4)).saturating_sub(2);
        let line = Line::from(format!("{icon} {}", truncate(&toast.text, text_width)));
        let width = u16::try_from(line.width() + 4)
            .unwrap_or(u16::MAX)
            .min(width);

        let rect = Rect {
            x: area.right().saturating_sub(width),
            y,
            width,
            height: 3,
        }
        .intersection(area);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(line.style(Style::new().fg(color))).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(color))
                    .padding(Padding::horizontal(1)),
            ),
            rect,
        );
        y = y.saturating_add(3);
    }
}
