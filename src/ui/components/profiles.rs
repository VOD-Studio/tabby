use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    config::{Choice, Origin},
    format::truncate,
    ui::{
        components::{popup, scrollbar},
        hit::Target,
        text::wrap,
    },
};

/// Widest the popup gets, borders included.
const WIDTH: u16 = 64;

const TITLE: &str = "切换配置";

/// The profile popup over `area`: every connection [`App::profiles`]
/// offers, one a row, the one in use marked; or, with no profile in
/// `config.toml`, how to add one.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if app.profiles.is_empty() {
        render_empty(frame, area, app);
        return;
    }
    let theme = app.theme;
    let profiles = &app.profiles;
    let len = profiles.choices.len();
    let height = u16::try_from(len + 2).unwrap_or(u16::MAX);
    let rect = popup::modal(app, area, WIDTH.min(area.width), height);
    let inner = popup::panel(frame, rect, theme.accent, Some(TITLE), None);

    let width = usize::from(inner.width);
    // The marker column, then the rest.
    let room = width.saturating_sub(2);
    // Names line up, but leave the addresses half the room at least.
    let name_width = profiles
        .choices
        .iter()
        .filter(|choice| choice.origin == Origin::Profile)
        .map(|choice| choice.name.width())
        .max()
        .unwrap_or(0)
        .min(room / 2);
    let window = app
        .profile_scroll
        .window(app.profile_cursor, len, usize::from(inner.height));
    scrollbar::list(
        frame,
        &app.hits,
        scrollbar::in_popup(inner),
        &window,
        len,
        theme.accent,
    );
    for (row, index) in window.enumerate() {
        let choice = &profiles.choices[index];
        let marker = if index == profiles.current {
            Span::styled(
                format!("{} ", app.icons.check),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![marker];
        spans.extend(describe(choice, room, name_width, app));
        let rect = Rect {
            y: inner.y + u16::try_from(row).unwrap_or(u16::MAX),
            height: 1,
            ..inner
        };
        app.hits.add(rect, Target::Profile(index));
        frame.render_widget(Line::from(spans), rect);
        if index == app.profile_cursor {
            theme.paint_cursor(frame.buffer_mut(), rect);
        }
    }
}

/// One choice in `room` columns: a profile's name in a column `name_width`
/// wide, then its address; the connection tabby started with, muted and
/// whole.
fn describe(choice: &Choice, room: usize, name_width: usize, app: &App) -> Vec<Span<'static>> {
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    if choice.origin != Origin::Profile {
        let text = format!("{}（{}）", choice.label(), choice.url);
        return vec![Span::styled(truncate(&text, room), muted)];
    }
    let name = truncate(&choice.name, name_width.max(1).min(room));
    let pad = name_width.saturating_sub(name.width());
    let url_room = room.saturating_sub(name_width + 2);
    let mut spans = vec![Span::styled(
        format!("{name}{}", " ".repeat(pad)),
        Style::new().fg(theme.fg),
    )];
    if url_room > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(truncate(&choice.url, url_room), muted));
    }
    spans
}

/// No profile to offer: says so, where `config.toml` is and what a profile
/// looks like.
fn render_empty(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let width = WIDTH.min(area.width);
    // Borders and the panel's padding take two columns a side.
    let text_width = usize::from(width.saturating_sub(4)).max(1);
    let fg = Style::new().fg(theme.fg);
    let muted = Style::new().fg(theme.muted);
    let accent = Style::new().fg(theme.accent);
    let key = accent.add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::raw(""),
        Line::styled("config.toml 中没有 profile。", fg),
        Line::raw(""),
        Line::styled("配置文件：", muted),
    ];
    let path = match &app.profiles.path {
        Some(path) => Span::styled(path.display().to_string(), fg),
        None => Span::styled("找不到主目录，无法确定位置", muted),
    };
    lines.extend(wrap(vec![path], text_width).into_iter().map(Line::from));
    lines.extend([
        Line::raw(""),
        Line::styled("在其中添加 profile，例如：", muted),
        Line::raw(""),
        Line::styled("[profiles.local]", accent),
        Line::styled("url = \"http://127.0.0.1:9090\"", accent),
        Line::styled("secret = \"…\"", accent),
        Line::raw(""),
        Line::from(vec![Span::styled("Esc", key), Span::styled(" 关闭", muted)]).centered(),
    ]);

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = popup::modal(app, area, width, height);
    let inner = popup::panel(frame, rect, theme.accent, Some(TITLE), None);
    frame.render_widget(Paragraph::new(lines), inner);
}
