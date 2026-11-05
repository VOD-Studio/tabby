use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};

use crate::{
    app::{App, ToastKind},
    format::truncate,
    ui::components::popup,
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
        let inner = popup::panel(frame, rect, color, None, None);
        frame.render_widget(Paragraph::new(line.style(Style::new().fg(color))), inner);
        y = y.saturating_add(3);
    }
}
