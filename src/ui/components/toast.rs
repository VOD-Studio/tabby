use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::{App, ToastKind},
    ui::{
        components::popup,
        text::{truncate_spans, wrap},
    },
};

/// Widest toast, borders included.
const MAX_WIDTH: u16 = 50;

/// Most lines of text a toast shows; what does not fit ends in `…`.
const MAX_LINES: usize = 3;

/// Toasts stacked down from the top-right corner, newest last. A text too
/// long for one line goes over several: what a toast says is what to do
/// next as often as what happened.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let mut y = area.y;
    for toast in &app.toasts {
        let (icon, color) = match toast.kind {
            ToastKind::Success => (app.icons.check, theme.success),
            ToastKind::Error => (app.icons.cross, theme.error),
        };
        let width = MAX_WIDTH.min(area.width);
        // Two border columns and one column of padding on each side.
        let text_width = usize::from(width.saturating_sub(4)).max(1);
        let mut lines = wrap(
            vec![Span::raw(format!("{icon} {}", toast.text))],
            text_width,
        );
        if lines.len() > MAX_LINES {
            lines.truncate(MAX_LINES);
            if let Some(last) = lines.last_mut() {
                last.push(Span::raw("…"));
                *last = truncate_spans(std::mem::take(last), text_width);
            }
        }
        let lines: Vec<Line> = lines.into_iter().map(Line::from).collect();
        let widest = lines.iter().map(Line::width).max().unwrap_or(0);
        let width = u16::try_from(widest + 4).unwrap_or(u16::MAX).min(width);
        let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);

        let rect = Rect {
            x: area.right().saturating_sub(width),
            y,
            width,
            height,
        }
        .intersection(area);
        let inner = popup::panel(frame, rect, color, None, None);
        frame.render_widget(Paragraph::new(lines).style(Style::new().fg(color)), inner);
        y = y.saturating_add(height);
    }
}
