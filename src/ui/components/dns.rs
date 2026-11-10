use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, dns},
    core::DNS_KEYS,
    format::truncate,
    ui::components::{popup, scrollbar},
};

/// Widest the popup gets, borders included.
const WIDTH: u16 = 76;

/// Columns the label takes (`代理节点 DNS` is the widest, at 12), and the
/// key after it.
const LABEL: usize = 14;
const KEY: usize = 25;

/// The key column is left out below this many columns of room.
const KEY_MIN_ROOM: usize = 64;

/// The DNS popup over `area`: every key a managed core can override, with
/// what it is set to, or that the subscription's value stands. The key under
/// the cursor takes the typing, while there is some.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let view = &app.dns_view;
    let overrides = app.dns_overrides();
    let len = DNS_KEYS.len();
    let height = u16::try_from(len + 2).unwrap_or(u16::MAX);
    let rect = popup::modal(app, area, WIDTH.min(area.width), height);
    let status = view
        .checking
        .then(|| Span::styled("校验中…", Style::new().fg(theme.muted)));
    let inner = popup::panel(frame, rect, theme.accent, Some("DNS 覆盖"), status);

    let room = usize::from(inner.width);
    let key_column = room >= KEY_MIN_ROOM;
    let before = LABEL + if key_column { KEY } else { 0 };
    let value_room = room.saturating_sub(before);
    let muted = Style::new().fg(theme.muted);

    let window = view
        .scroll
        .window(view.cursor, len, usize::from(inner.height));
    scrollbar::list(
        frame,
        &app.hits,
        scrollbar::in_popup(inner),
        &window,
        len,
        theme.accent,
    );
    for (row, index) in window.enumerate() {
        let key = DNS_KEYS[index];
        let label = dns::label(key);
        let mut spans = vec![
            Span::styled(label, Style::new().fg(theme.fg)),
            Span::raw(" ".repeat(LABEL.saturating_sub(label.width()))),
        ];
        if key_column {
            spans.push(Span::styled(key, muted));
            spans.push(Span::raw(" ".repeat(KEY.saturating_sub(key.width()))));
        }
        let y = inner.y + u16::try_from(row).unwrap_or(u16::MAX);
        let rect = Rect {
            y,
            height: 1,
            ..inner
        };
        let editing = (index == view.cursor)
            .then_some(view.editing.as_ref())
            .flatten();
        if let Some(input) = editing {
            let shown = value_room.saturating_sub(1).max(1);
            let scroll = input.visual_scroll(shown);
            let text: String = input.value().chars().skip(scroll).collect();
            spans.push(Span::styled(
                truncate(&text, shown),
                Style::new().fg(theme.fg).add_modifier(Modifier::UNDERLINED),
            ));
            let caret = before + input.visual_cursor().saturating_sub(scroll);
            frame.set_cursor_position((inner.x + u16::try_from(caret).unwrap_or(u16::MAX), y));
        } else {
            match overrides.get(key) {
                Some(value) => spans.push(Span::styled(
                    truncate(&dns::show(value), value_room),
                    Style::new().fg(theme.accent),
                )),
                None => spans.push(Span::styled("跟随订阅", muted)),
            }
        }
        frame.render_widget(Line::from(spans), rect);
        if index == view.cursor {
            theme.paint_cursor(frame.buffer_mut(), rect);
        }
    }
}
