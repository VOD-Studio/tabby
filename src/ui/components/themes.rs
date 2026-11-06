use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    format::truncate,
    themes::{Entry, Source},
    ui::{components::popup, hit::Target},
};

/// Widest the popup gets, borders included.
const WIDTH: u16 = 64;

const TITLE: &str = "切换主题";

/// The theme picker over `area`: every theme [`App::themes`] offers, one a
/// row, the one in use marked, where each comes from or why it cannot be
/// used.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let picker = &app.themes;
    let len = picker.entries.len();
    let height = u16::try_from(len + 2).unwrap_or(u16::MAX);
    let rect = popup::modal(app, area, WIDTH.min(area.width), height);
    let inner = popup::panel(frame, rect, theme.accent, Some(TITLE), None);

    let width = usize::from(inner.width);
    // The marker column, then the rest.
    let room = width.saturating_sub(2);
    // Names line up, but leave the details half the room at least.
    let name_width = picker
        .entries
        .iter()
        .map(|entry| entry.id.width())
        .max()
        .unwrap_or(0)
        .min(room / 2);
    let window = picker
        .scroll
        .window(picker.cursor, len, usize::from(inner.height));
    for (row, index) in window.enumerate() {
        let entry = &picker.entries[index];
        let marker = if entry.id == app.settings.theme {
            Span::styled(
                format!("{} ", app.icons.check),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![marker];
        spans.extend(describe(entry, room, name_width, app));
        let rect = Rect {
            y: inner.y + u16::try_from(row).unwrap_or(u16::MAX),
            height: 1,
            ..inner
        };
        app.hits.add(rect, Target::Theme(index));
        frame.render_widget(Line::from(spans), rect);
        if index == picker.cursor {
            theme.paint_cursor(frame.buffer_mut(), rect);
        }
    }
}

/// One theme in `room` columns: its name in a column `name_width` wide,
/// then where it comes from; a theme that cannot be used is muted, and
/// says why in place of where.
fn describe(entry: &Entry, room: usize, name_width: usize, app: &App) -> Vec<Span<'static>> {
    let theme = app.theme;
    let name = truncate(&entry.id, name_width.max(1).min(room));
    let pad = name_width.saturating_sub(name.width());
    let (name_color, detail, detail_color) = match (&entry.theme, &entry.source) {
        (Err(reason), _) => {
            // A parse error runs over lines; the first says what and where.
            let reason = reason.lines().next().unwrap_or_default();
            (theme.muted, format!("无效：{reason}"), theme.error)
        }
        (Ok(_), Source::Builtin) => (theme.fg, "内置".to_owned(), theme.muted),
        (Ok(_), Source::File(path)) => {
            let shown = if path.parent() == app.themes.dir.as_deref() {
                "自定义".to_owned()
            } else {
                "文件".to_owned()
            };
            (theme.fg, shown, theme.muted)
        }
    };
    let mut spans = vec![Span::styled(
        format!("{name}{}", " ".repeat(pad)),
        Style::new().fg(name_color),
    )];
    let detail_room = room.saturating_sub(name_width + 2);
    if detail_room > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            truncate(&detail, detail_room),
            Style::new().fg(detail_color),
        ));
    }
    spans
}
