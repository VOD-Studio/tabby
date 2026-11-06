use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models::{Delay, Proxies, Proxy},
    app::{App, Pane, Sort},
    format::truncate,
    ui::{
        NARROW_WIDTH,
        components::{popup, states, title},
        text::truncate_spans,
    },
};

/// Columns of the group list panel, borders included.
const GROUPS_WIDTH: u16 = 28;

/// Rows one group takes in the list: name line and current-node line.
const GROUP_ROWS: u16 = 2;

/// Rows of the group picker that stands in for the list when stacked.
const PICKER_ROWS: u16 = 1;

/// Widest the picker's drop-down gets, borders included.
const DROPDOWN_WIDTH: u16 = 64;

/// The least room a current node is cut down to in a group summary before
/// it is left out: `  → ` and four columns of its name.
const NODE_MIN_WIDTH: usize = 8;

/// Whether a page `width` columns wide folds the group list into a picker
/// row above the grid (plan §7.3).
pub fn stacked(width: u16) -> bool {
    width < NARROW_WIDTH
}

/// Where the page's parts go.
enum Panes {
    /// The group list on the left of the grid.
    Side { groups: Rect, nodes: Rect },
    /// The picker row above the grid.
    Stacked { picker: Rect, nodes: Rect },
}

fn panes(area: Rect) -> Panes {
    if stacked(area.width) {
        let [picker, nodes] =
            Layout::vertical([Constraint::Length(PICKER_ROWS), Constraint::Fill(1)]).areas(area);
        Panes::Stacked { picker, nodes }
    } else {
        let [groups, nodes] =
            Layout::horizontal([Constraint::Length(GROUPS_WIDTH), Constraint::Fill(1)]).areas(area);
        Panes::Side { groups, nodes }
    }
}

/// What a body leaves the picker's drop-down: everything below the picker
/// row.
fn below_picker(body: Rect) -> Rect {
    let [_, below] =
        Layout::vertical([Constraint::Length(PICKER_ROWS), Constraint::Fill(1)]).areas(body);
    below
}

/// The drop-down listing `len` groups, hanging from the picker row of
/// `body`.
fn dropdown(body: Rect, len: usize) -> Rect {
    let below = below_picker(body);
    Rect {
        width: below.width.min(DROPDOWN_WIDTH),
        height: u16::try_from(len + 2).unwrap_or(u16::MAX).min(below.height),
        ..below
    }
}

/// What a panel leaves for its content, whatever its title.
fn panel_inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

fn group_capacity(inner: Rect) -> usize {
    usize::from(inner.height / GROUP_ROWS)
}

/// Cards per row in a node panel `width` columns wide.
fn grid_columns(width: u16) -> u16 {
    (width / CARD_MIN_WIDTH).clamp(1, MAX_COLUMNS)
}

fn grid_rows(inner: Rect) -> usize {
    usize::from(inner.height / CARD_ROWS)
}

/// How much of the page fits on screen, for paging the cursor.
#[derive(Debug, Clone, Copy)]
pub struct Fit {
    /// The group list is folded into the picker row.
    pub stacked: bool,
    /// Groups visible in the list, or in the picker's drop-down at its
    /// tallest.
    pub groups: usize,
    /// Cards per row.
    pub columns: usize,
    /// Card rows visible in the grid.
    pub rows: usize,
}

/// Measures the page as [`render`] and [`render_picker`] lay it out in
/// `body`.
pub fn fit(body: Rect) -> Fit {
    let (stacked, groups, nodes) = match panes(body) {
        Panes::Side { groups, nodes } => (false, group_capacity(panel_inner(groups)), nodes),
        Panes::Stacked { nodes, .. } => {
            let rows = below_picker(body).height.saturating_sub(2);
            (true, usize::from(rows), nodes)
        }
    };
    let nodes = panel_inner(nodes);
    Fit {
        stacked,
        groups,
        columns: usize::from(grid_columns(nodes.width)),
        rows: grid_rows(nodes),
    }
}

/// The proxies page: group list on the left of the selected group's nodes,
/// or, when narrow, the group picker above them.
pub fn render(frame: &mut Frame, area: Rect, app: &App, proxies: &Proxies) {
    let groups: Vec<&Proxy> = app.visible_groups().collect();
    if groups.is_empty() {
        states::empty(frame, area, app, "配置里没有代理组", &[("r", "刷新")]);
        return;
    }
    let selected = app.proxies_view.group.min(groups.len() - 1);

    match panes(area) {
        Panes::Side {
            groups: left,
            nodes,
        } => {
            render_groups(frame, left, app, proxies, &groups, selected);
            render_nodes(frame, nodes, app, proxies, groups[selected]);
        }
        Panes::Stacked { picker, nodes } => {
            render_picker_row(frame, picker, app, proxies, groups[selected]);
            render_nodes(frame, nodes, app, proxies, groups[selected]);
        }
    }
}

/// The picker row: `▾`, then the selected group as [`summary`] gives it.
/// With the focus on it, it has the cursor's background.
fn render_picker_row(frame: &mut Frame, area: Rect, app: &App, proxies: &Proxies, group: &Proxy) {
    let theme = app.theme;
    let focused = app.proxies_view.focus == Pane::Groups;
    let (mark, name) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.muted, theme.fg)
    };
    let mut spans = vec![Span::styled(
        "▾ ",
        Style::new().fg(mark).add_modifier(Modifier::BOLD),
    )];
    let width = usize::from(area.width).saturating_sub(2);
    spans.extend(summary(
        app,
        proxies,
        group,
        width,
        Style::new().fg(name).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    if focused {
        theme.paint_cursor(frame.buffer_mut(), area);
    }
}

/// The picker's drop-down over the page `body`: every group the page lists,
/// the selected one ticked, the cursor on [`ProxiesView::picker`].
///
/// [`ProxiesView::picker`]: crate::app::ProxiesView::picker
pub fn render_picker(frame: &mut Frame, body: Rect, app: &App) {
    let Some(Ok(proxies)) = &app.proxies else {
        return;
    };
    let groups: Vec<&Proxy> = app.visible_groups().collect();
    if groups.is_empty() {
        return;
    }
    let theme = app.theme;
    let view = &app.proxies_view;
    let len = groups.len();
    let rect = dropdown(body, len);
    let inner = popup::panel(frame, rect, theme.accent, Some("代理组"), None);
    let cursor = view.picker.min(len - 1);
    let selected = view.group.min(len - 1);
    let width = usize::from(inner.width).saturating_sub(2);
    let window = view
        .picker_scroll
        .window(cursor, len, usize::from(inner.height));
    for (row, index) in window.enumerate() {
        let area = Rect {
            y: inner.y + u16::try_from(row).unwrap_or(0),
            height: 1,
            ..inner
        };
        let mark = if index == selected {
            app.icons.check
        } else {
            " "
        };
        let accent = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::styled(mark, accent), Span::styled(" ", accent)];
        let name = Style::new()
            .fg(theme.fg)
            .add_modifier(if index == selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        spans.extend(summary(app, proxies, groups[index], width, name));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        if index == cursor {
            theme.paint_cursor(frame.buffer_mut(), area);
        }
    }
}

/// A group on one line in `width` columns: `name  SEL  → node · 120 ms`.
/// Short of room the current node is cut, down to [`NODE_MIN_WIDTH`], then
/// left out; then the type; then the name is cut.
fn summary(
    app: &App,
    proxies: &Proxies,
    group: &Proxy,
    width: usize,
    name: Style,
) -> Vec<Span<'static>> {
    let muted = Style::new().fg(app.theme.muted);
    let head = vec![
        Span::styled(group.name.clone(), name),
        Span::styled(format!("  {}", kind_label(&group.kind)), muted),
    ];
    let head_width: usize = head.iter().map(Span::width).sum();
    if head_width > width {
        return truncate_spans(vec![head[0].clone()], width);
    }
    let mut node = vec![];
    if let Some(now) = &group.now {
        node.push(Span::styled(format!("  → {now}"), muted));
        if let Some((text, color)) = delay_label(app, proxies, group, now) {
            node.push(Span::styled(text, Style::new().fg(color)));
        }
    }
    let room = width - head_width;
    let node_width: usize = node.iter().map(Span::width).sum();
    let mut spans = head;
    if node_width <= room {
        spans.extend(node);
    } else if room >= NODE_MIN_WIDTH {
        spans.extend(truncate_spans(node, room));
    }
    spans
}

/// ` · 120 ms` after the current node of an auto-testing group, colored by
/// how fast it is; nothing for other groups or an untested node.
fn delay_label(app: &App, proxies: &Proxies, group: &Proxy, now: &str) -> Option<(String, Color)> {
    let theme = app.theme;
    (group.kind == "URLTest")
        .then(|| proxies.get(now).and_then(Proxy::delay))
        .flatten()
        .map(|delay| match delay {
            Delay::Millis(ms) => (format!(" · {ms} ms"), theme.delay(ms)),
            Delay::Failed => (" · 失败".to_owned(), theme.error),
            Delay::Timeout => (" · 超时".to_owned(), theme.error),
        })
}

/// A panel `width` columns wide, its border accent when it has the focus,
/// titled `left` and `right` as much as fits.
fn panel(
    app: &App,
    focused: bool,
    width: u16,
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
) -> Block<'static> {
    let color = if focused {
        app.theme.accent
    } else {
        app.theme.muted
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color));
    title::titled(block, width, left, right)
}

fn render_groups(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    proxies: &Proxies,
    groups: &[&Proxy],
    selected: usize,
) {
    let theme = app.theme;
    let focused = app.proxies_view.focus == Pane::Groups;
    let heading = vec![
        Span::styled(
            " 代理组",
            Style::new()
                .fg(if focused { theme.accent } else { theme.fg })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {} ", groups.len()), Style::new().fg(theme.muted)),
    ];
    let block = panel(app, focused, area.width, heading, vec![]);
    let inner = panel_inner(area);
    frame.render_widget(block, area);

    let capacity = group_capacity(inner);
    let window = app
        .proxies_view
        .group_scroll
        .window(selected, groups.len(), capacity);
    for (row, index) in window.enumerate() {
        let rect = Rect {
            y: inner.y + u16::try_from(row).unwrap_or(0) * GROUP_ROWS,
            height: GROUP_ROWS,
            ..inner
        };
        let is_selected = index == selected;
        let lines = group_item(
            app,
            proxies,
            groups[index],
            usize::from(rect.width),
            is_selected,
        );
        frame.render_widget(Paragraph::new(lines), rect);
        if is_selected {
            theme.paint_cursor(frame.buffer_mut(), rect);
            // The accent bar stays one: reverse video would swap its halves.
            let bar = Rect { width: 1, ..rect };
            frame.buffer_mut().set_style(
                bar,
                Style::new()
                    .fg(theme.accent)
                    .remove_modifier(Modifier::REVERSED),
            );
        }
    }
}

/// Name and type on the first line, `→ current node` (and its delay for
/// auto-testing groups) on the second.
fn group_item(
    app: &App,
    proxies: &Proxies,
    group: &Proxy,
    width: usize,
    selected: bool,
) -> Vec<Line<'static>> {
    let theme = app.theme;
    let label = kind_label(&group.kind);
    let bar = if selected { "▌" } else { " " };

    let name = truncate(&group.name, width.saturating_sub(2 + label.width() + 1));
    let padding = width.saturating_sub(2 + name.width() + label.width());
    let first = Line::from(vec![
        Span::styled(bar, Style::new().fg(theme.accent)),
        Span::raw(" "),
        Span::styled(
            name,
            Style::new().fg(theme.fg).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::raw(" ".repeat(padding)),
        Span::styled(label, Style::new().fg(theme.muted)),
    ]);

    let mut second = vec![Span::styled(bar, Style::new().fg(theme.accent))];
    if let Some(now) = &group.now {
        let delay = delay_label(app, proxies, group, now);
        let delay_width = delay.as_ref().map_or(0, |(text, _)| text.width());
        second.push(Span::styled(
            format!(
                " → {}",
                truncate(now, width.saturating_sub(4 + delay_width))
            ),
            Style::new().fg(theme.muted),
        ));
        if let Some((text, color)) = delay {
            second.push(Span::styled(text, Style::new().fg(color)));
        }
    }
    vec![first, Line::from(second)]
}

/// Narrowest card that still fits a name and a delay.
const CARD_MIN_WIDTH: u16 = 26;
const MAX_COLUMNS: u16 = 4;

/// Rows per card: top border, name, details, bottom border (which doubles as
/// the delay bar).
const CARD_ROWS: u16 = 4;

/// Delay at which the bar is full; slower nodes are as bad as this one.
const BAR_FULL_MS: usize = 500;

fn render_nodes(frame: &mut Frame, area: Rect, app: &App, proxies: &Proxies, group: &Proxy) {
    let theme = app.theme;
    let view = &app.proxies_view;
    let focused = view.focus == Pane::Nodes;
    let members = app.visible_nodes();
    let total = group.all.as_ref().map_or(0, Vec::len);
    let filter = view.filter.value();

    let mut details = if filter.is_empty() {
        format!("  {} · {total} 个节点", group.kind)
    } else {
        format!(
            "  {} · {}/{total} 个节点 · 过滤「{filter}」",
            group.kind,
            members.len()
        )
    };
    details.push_str(match view.sort {
        Sort::Default => " · 默认顺序 ",
        Sort::Delay => " · 按延迟 ↑ ",
        Sort::Name => " · 按名称 ↑ ",
    });
    let heading = vec![
        Span::styled(
            format!(" {}", group.name),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(details, Style::new().fg(theme.muted)),
    ];
    let inner = panel_inner(area);
    if members.is_empty() {
        frame.render_widget(panel(app, focused, area.width, heading, vec![]), area);
        if total == 0 {
            states::empty(frame, inner, app, "该组没有节点", &[("r", "刷新")]);
        } else {
            states::empty(
                frame,
                inner,
                app,
                &format!("没有名称含「{filter}」的节点"),
                &[("Esc", "清除过滤"), ("/", "修改过滤")],
            );
        }
        return;
    }

    let columns = grid_columns(inner.width);
    let card_width = inner.width / columns;
    let columns = usize::from(columns);
    let total_rows = members.len().div_ceil(columns);
    let cursor = app.node_cursor();
    let rows = view
        .node_scroll
        .window(cursor / columns, total_rows, grid_rows(inner));

    let first = rows.start * columns + 1;
    let last = (rows.end * columns).min(members.len());
    let mut counter = format!(" {first}–{last} / {} ", members.len());
    if rows.start > 0 {
        counter.insert(1, '▴');
    }
    if rows.end < total_rows {
        counter.push('▾');
    }
    let counter = vec![Span::styled(counter, Style::new().fg(theme.muted))];
    frame.render_widget(panel(app, focused, area.width, heading, counter), area);

    for (row, row_index) in rows.enumerate() {
        for column in 0..columns {
            let index = row_index * columns + column;
            let Some(&name) = members.get(index) else {
                break;
            };
            let column_x = u16::try_from(column).unwrap_or(0) * card_width;
            let rect = Rect {
                x: inner.x + column_x,
                y: inner.y + u16::try_from(row).unwrap_or(0) * CARD_ROWS,
                width: card_width,
                height: CARD_ROWS,
            };
            let card = Card {
                name,
                proxy: proxies.get(name),
                current: group.now.as_deref() == Some(name),
                cursor: focused && index == cursor,
            };
            render_card(frame, rect, app, &card);
        }
    }
}

struct Card<'a> {
    name: &'a str,
    proxy: Option<&'a Proxy>,
    /// The group routes through this node.
    current: bool,
    /// The cursor is on this card.
    cursor: bool,
}

fn render_card(frame: &mut Frame, rect: Rect, app: &App, card: &Card) {
    let theme = app.theme;
    // The cursor is a heavy accent frame: a change of shape, so it shows in
    // any palette, and it leaves the body's own colors (the delay above all)
    // as they are. The node in use is told by its accent name and the ✓.
    let (border_type, border) = if card.cursor {
        (BorderType::Thick, Style::new().fg(theme.accent))
    } else {
        (BorderType::Rounded, Style::new().fg(theme.muted))
    };
    let block = Block::bordered()
        .border_type(border_type)
        .border_style(border);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let width = usize::from(inner.width);

    let mark = if card.current { app.icons.check } else { " " };
    let name = truncate(card.name, width.saturating_sub(2));
    let padding = width.saturating_sub(name.width() + 1);
    let name_style = match (card.current, card.cursor) {
        (true, _) => Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        (false, true) => Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        (false, false) => Style::new().fg(theme.fg),
    };
    let title = Line::from(vec![
        Span::styled(name, name_style),
        Span::raw(" ".repeat(padding)),
        Span::styled(
            mark,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
    ]);

    let testing = app.testing.contains(card.name);
    let delay = card.proxy.and_then(Proxy::delay).filter(|_| !testing);
    let (delay_text, delay_style) = match delay {
        _ if testing => (
            format!("{} 测试中", states::spinner_frame(app)),
            Style::new().fg(theme.muted),
        ),
        None => ("—".to_owned(), Style::new().fg(theme.muted)),
        Some(Delay::Millis(ms)) => (
            format!("{ms} ms"),
            Style::new()
                .fg(theme.delay(ms))
                .add_modifier(Modifier::BOLD),
        ),
        Some(Delay::Failed) => (
            "失败".to_owned(),
            Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
        ),
        Some(Delay::Timeout) => (
            "超时".to_owned(),
            Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
        ),
    };
    let kind = card.proxy.map_or_else(String::new, |proxy| {
        if proxy.udp {
            format!("{} · UDP", proxy.kind)
        } else {
            proxy.kind.clone()
        }
    });
    let kind = truncate(&kind, width.saturating_sub(delay_text.width() + 1));
    let gap = width.saturating_sub(kind.width() + delay_text.width());
    let details = Line::from(vec![
        Span::styled(kind, Style::new().fg(theme.muted)),
        Span::raw(" ".repeat(gap)),
        Span::styled(delay_text, delay_style),
    ]);
    frame.render_widget(Paragraph::new(vec![title, details]), inner);

    // The bottom border becomes the delay bar.
    let bar_width = usize::from(rect.width.saturating_sub(2));
    let filled = match delay {
        Some(Delay::Millis(ms)) => {
            (usize::try_from(ms).unwrap_or(BAR_FULL_MS).min(BAR_FULL_MS) * bar_width / BAR_FULL_MS)
                .max(1)
        }
        _ => 0,
    };
    let bar_color = match delay {
        Some(Delay::Millis(ms)) => theme.delay(ms),
        _ => theme.muted,
    };
    let symbols = border_type.to_border_set();
    let bottom = Line::from(vec![
        Span::styled(symbols.bottom_left, border),
        Span::styled("━".repeat(filled), Style::new().fg(bar_color)),
        Span::styled(
            symbols
                .horizontal_bottom
                .repeat(bar_width - filled.min(bar_width)),
            border,
        ),
        Span::styled(symbols.bottom_right, border),
    ]);
    let bottom_row = Rect {
        y: rect.bottom().saturating_sub(1),
        height: 1,
        ..rect
    };
    frame.render_widget(Paragraph::new(bottom), bottom_row);
}

/// Short group-type tag shown in the list.
fn kind_label(kind: &str) -> String {
    match kind {
        "Selector" => "SEL".to_owned(),
        "URLTest" => "URL".to_owned(),
        "Fallback" => "FB".to_owned(),
        "LoadBalance" => "LB".to_owned(),
        "Relay" => "REL".to_owned(),
        other => other.chars().take(3).collect::<String>().to_uppercase(),
    }
}
