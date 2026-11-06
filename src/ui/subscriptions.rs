use jiff::{Timestamp, Zoned};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models,
    app::App,
    core::{Kind, Subscription},
    format,
    ui::{
        components::{states, title},
        hit::Target,
        providers::usage,
        text,
    },
};

/// Columns between the cells of a row.
const GAP: usize = 2;

/// The mark and the space after it, in front of every name.
const MARK_WIDTH: usize = 2;

/// A name is never squeezed below this (or its own width, if shorter).
const NAME_MIN: usize = 8;

/// A name wider than this is cut, so the columns after it keep their room.
const NAME_MAX: usize = 24;

/// Narrower than this, the usage is left out.
const USAGE_MIN: usize = 12;

/// The width of the kind tag, `URL` or `文件`.
const KIND_WIDTH: usize = 4;

const NEVER: &str = "—";

fn panel() -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded)
}

/// Rows that fit in a page body `body`: the panel's inside.
pub fn capacity(body: Rect) -> usize {
    usize::from(panel().inner(body).height)
}

/// The page: the subscriptions of the managed kernel, one a row, the one in
/// use marked; or, on an external controller, why there are none.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if !app.is_managed() {
        states::empty(
            frame,
            area,
            app,
            "连接的是外部控制器，订阅由它自己管理",
            &[("P", "切换到托管内核")],
        );
        return;
    }
    let view = &app.subscriptions_view;
    match &view.list {
        None => states::loading(frame, area, app, "正在读取订阅…"),
        Some(Err(err)) => states::error_lines(
            frame,
            area,
            app,
            &text::core_title(err),
            &text::core_detail(err),
        ),
        Some(Ok(store)) => draw(frame, area, app, store.subscriptions.len()),
    }
}

fn draw(frame: &mut Frame, area: Rect, app: &App, total: usize) {
    let theme = app.theme;
    let view = &app.subscriptions_view;
    let visible = view.visible();
    let filter = view.filter.value();

    let count = if filter.is_empty() {
        format!("  {total} ")
    } else {
        format!("  {}/{total} · 过滤「{filter}」 ", visible.len())
    };
    let right = if view.importing {
        vec![Span::styled(
            format!(" {} 正在导入… ", states::spinner_frame(app)),
            Style::new().fg(theme.accent),
        )]
    } else {
        vec![]
    };
    let block = title::titled(
        panel().border_style(Style::new().fg(theme.accent)),
        area.width,
        vec![
            Span::styled(
                " 订阅",
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(count, Style::new().fg(theme.muted)),
        ],
        right,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hits.add(inner, Target::List);

    if visible.is_empty() {
        if total == 0 {
            states::empty(
                frame,
                inner,
                app,
                "还没有订阅",
                &[("a", "添加订阅地址或文件")],
            );
        } else {
            states::empty(
                frame,
                inner,
                app,
                &format!("没有匹配「{filter}」的订阅"),
                &[("Esc", "清除过滤"), ("/", "修改过滤")],
            );
        }
        return;
    }

    let now = Zoned::now();
    let times: Vec<String> = visible.iter().map(|sub| updated(sub, &now)).collect();
    let autos: Vec<String> = visible.iter().map(|sub| auto_label(sub)).collect();
    let layout = fit(
        usize::from(inner.width),
        visible
            .iter()
            .map(|sub| sub.name.width())
            .max()
            .unwrap_or(0),
        times.iter().map(|time| time.width()).max().unwrap_or(0),
        autos.iter().map(|auto| auto.width()).max().unwrap_or(0),
        visible.iter().any(|sub| sub.usage.is_some()),
    );
    let cursor = view.cursor.min(visible.len() - 1);
    let window = view.scroll.window(cursor, visible.len(), capacity(area));
    for (row, index) in window.enumerate() {
        let sub = visible[index];
        let rect = Rect {
            y: inner.y + u16::try_from(row).unwrap_or(u16::MAX),
            height: 1,
            ..inner
        };
        app.hits.add(rect, Target::Subscription(sub.id.clone()));

        let line = row_line(app, sub, (&times[index], &autos[index]), layout, &now);
        frame.render_widget(line, rect);
        if index == cursor {
            theme.paint_cursor(frame.buffer_mut(), rect);
        }
    }
}

/// One row: the mark, the name, then the cells that fit.
fn row_line(
    app: &App,
    sub: &Subscription,
    (time, auto): (&str, &str),
    layout: Layout,
    now: &Zoned,
) -> Line<'static> {
    let theme = app.theme;
    let view = &app.subscriptions_view;
    let muted = Style::new().fg(theme.muted);
    let in_use = view.active() == Some(sub.id.as_str());
    let mark = if view.busy.contains(&sub.id) {
        Span::styled(
            format!("{} ", states::spinner_frame(app)),
            Style::new().fg(theme.accent),
        )
    } else if in_use {
        Span::styled(
            format!("{} ", app.icons.check),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("  ")
    };
    let name_style = if in_use {
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.fg)
    };
    let mut spans = vec![mark, cell(&sub.name, layout.name, name_style)];
    if layout.kind {
        let (label, color) = match sub.kind {
            Kind::Url => ("URL", theme.c2),
            Kind::File => ("文件", theme.c1),
        };
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(cell(label, KIND_WIDTH, Style::new().fg(color)));
    }
    if layout.time > 0 {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(cell(time, layout.time, muted));
    }
    if layout.auto > 0 {
        spans.push(Span::raw(" ".repeat(GAP)));
        // Off is what stands out: the rest is as it was set.
        let style = if sub.auto_update || sub.kind == Kind::File {
            muted
        } else {
            Style::new().fg(theme.warning)
        };
        spans.push(cell(auto, layout.auto, style));
    }
    if layout.usage > 0
        && let Some(used) = sub.usage
    {
        spans.push(Span::raw(" ".repeat(GAP)));
        let used = models::Subscription {
            upload: used.upload,
            download: used.download,
            total: used.total,
            expire: used.expire,
        };
        spans.push(Span::styled(
            format::truncate(&usage(used, now), layout.usage),
            muted,
        ));
    }
    Line::from(spans)
}

/// How a subscription is kept up to date: `24h` for the interval it is
/// asked at, `关` when it is not, `—` for a file, which only `u` reads
/// again.
fn auto_label(sub: &Subscription) -> String {
    match (sub.kind, sub.auto_update) {
        (Kind::File, _) => NEVER.to_owned(),
        (Kind::Url, false) => "关".to_owned(),
        (Kind::Url, true) => format!("{}h", sub.interval_hours.max(1)),
    }
}

/// When a subscription was last imported or updated.
fn updated(sub: &Subscription, now: &Zoned) -> String {
    Timestamp::from_second(sub.updated_at)
        .ok()
        .filter(|_| sub.updated_at > 0)
        .map_or_else(|| NEVER.to_owned(), |then| format::ago(then, now))
}

/// `text` cut to `width` and padded to it.
fn cell(text: &str, width: usize, style: Style) -> Span<'static> {
    let text = format::truncate(text, width);
    let pad = " ".repeat(width.saturating_sub(text.width()));
    Span::styled(format!("{text}{pad}"), style)
}

/// Where the cells of a row go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    name: usize,
    /// Whether the kind tag fits.
    kind: bool,
    /// The width of the update time; 0 leaves it out.
    time: usize,
    /// The width of the auto-update state; 0 leaves it out.
    auto: usize,
    /// Room for the usage, after everything else; 0 leaves it out.
    usage: usize,
}

/// Fits a row `width` columns wide: a name as wide as `name` (up to
/// [`NAME_MAX`]); then the auto-update state, the time and the kind, dropped
/// in that order while the name would get fewer than [`NAME_MIN`] columns;
/// then, if everything else fits and at least [`USAGE_MIN`] columns are
/// left, the usage.
fn fit(width: usize, name: usize, time: usize, auto: usize, has_usage: bool) -> Layout {
    let name = name.min(NAME_MAX);
    let cost = |kind: bool, time: usize, auto: usize| -> usize {
        MARK_WIDTH
            + if kind { GAP + KIND_WIDTH } else { 0 }
            + if time > 0 { GAP + time } else { 0 }
            + if auto > 0 { GAP + auto } else { 0 }
    };
    let (mut kind, mut time, mut auto) = (true, time, auto);
    let squeezed = |kind, time, auto| cost(kind, time, auto) + name.min(NAME_MIN) > width;
    if squeezed(kind, time, auto) {
        auto = 0;
    }
    if squeezed(kind, time, auto) {
        time = 0;
    }
    if squeezed(kind, time, auto) {
        kind = false;
    }
    let used = cost(kind, time, auto);
    let name = name.min(width.saturating_sub(used));
    let rest = width.saturating_sub(used + name + GAP);
    Layout {
        name,
        kind,
        time,
        auto,
        usage: if has_usage && rest >= USAGE_MIN {
            rest
        } else {
            0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_columns_on_the_right_go_before_the_name_is_squeezed() {
        // Everything fits: the name as wide as it is, the usage gets the rest.
        let wide = fit(100, 10, 8, 4, true);
        assert_eq!(
            wide,
            Layout {
                name: 10,
                kind: true,
                time: 8,
                auto: 4,
                // 100 less the mark, the kind, the time and the state (24),
                // the name (10) and the gap before the usage.
                usage: 64,
            }
        );
        // Too narrow for the usage: nothing is left over to give it.
        assert_eq!(fit(46, 10, 8, 4, true).usage, 0);
        // The state goes first, then the time, then the kind.
        let (a, b, c) = (
            fit(30, 20, 8, 4, false),
            fit(24, 20, 8, 4, false),
            fit(12, 20, 8, 4, false),
        );
        assert_eq!((a.auto, a.time, a.kind), (0, 8, true));
        assert_eq!((b.auto, b.time, b.kind), (0, 0, true));
        assert_eq!((c.auto, c.time, c.kind), (0, 0, false));
        // And a name is never wider than the room it has.
        assert_eq!(c.name, 10);
        // A name wider than NAME_MAX is cut so the columns keep their room.
        assert_eq!(fit(100, 60, 8, 4, false).name, NAME_MAX);
    }

    #[test]
    fn the_state_says_the_interval_or_off_or_that_a_file_is_read_by_hand() {
        let sub = |kind, auto, hours| Subscription {
            id: "a".to_owned(),
            name: "a".to_owned(),
            kind,
            source: String::new(),
            auto_update: auto,
            interval_hours: hours,
            updated_at: 0,
            usage: None,
        };
        assert_eq!(auto_label(&sub(Kind::Url, true, 24)), "24h");
        assert_eq!(auto_label(&sub(Kind::Url, true, 0)), "1h");
        assert_eq!(auto_label(&sub(Kind::Url, false, 24)), "关");
        assert_eq!(auto_label(&sub(Kind::File, true, 24)), "—");
    }
}
