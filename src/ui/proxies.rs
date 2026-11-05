use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models::{Proxies, Proxy},
    app::{App, Pane, Sort},
    format::truncate,
    ui::components::states,
};

/// Columns of the group list panel, borders included.
const GROUPS_WIDTH: u16 = 28;

/// Rows one group takes in the list: name line and current-node line.
const GROUP_ROWS: u16 = 2;

/// The proxies page: group list on the left, the selected group's nodes on the
/// right.
pub fn render(frame: &mut Frame, area: Rect, app: &App, proxies: &Proxies) {
    let groups: Vec<&Proxy> = app.visible_groups().collect();
    if groups.is_empty() {
        states::empty(frame, area, app, "配置里没有代理组", &[("r", "刷新")]);
        return;
    }
    let selected = app.proxies_view.group.min(groups.len() - 1);

    let [left, right] =
        Layout::horizontal([Constraint::Length(GROUPS_WIDTH), Constraint::Fill(1)]).areas(area);
    render_groups(frame, left, app, proxies, &groups, selected);
    render_nodes(frame, right, app, proxies, groups[selected]);
}

fn panel(app: &App, focused: bool, title: Line<'static>) -> Block<'static> {
    let color = if focused {
        app.theme.accent
    } else {
        app.theme.muted
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(title)
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
    let title = Line::from(vec![
        Span::styled(
            " 代理组",
            Style::new()
                .fg(if focused { theme.accent } else { theme.fg })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {} ", groups.len()), Style::new().fg(theme.muted)),
    ]);
    let block = panel(app, focused, title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let capacity = usize::from(inner.height / GROUP_ROWS);
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
        let style = if is_selected {
            theme.surface
        } else {
            Style::new()
        };
        frame.render_widget(Paragraph::new(lines).style(style), rect);
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
        let delay = (group.kind == "URLTest")
            .then(|| proxies.get(now).and_then(Proxy::delay))
            .flatten()
            .map(|millis| match millis {
                0 => (" · 超时".to_owned(), theme.error),
                ms => (format!(" · {ms} ms"), theme.delay(ms)),
            });
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
    let name_span = Span::styled(
        format!(" {}", group.name),
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
    );
    let block = panel(
        app,
        focused,
        Line::from(vec![
            name_span,
            Span::styled(details, Style::new().fg(theme.muted)),
        ]),
    );
    let inner = block.inner(area);
    if members.is_empty() {
        frame.render_widget(block, area);
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

    let columns = (inner.width / CARD_MIN_WIDTH).clamp(1, MAX_COLUMNS);
    let card_width = inner.width / columns;
    let columns = usize::from(columns);
    let total_rows = members.len().div_ceil(columns);
    let cursor = app.node_cursor();
    let rows = view.node_scroll.window(
        cursor / columns,
        total_rows,
        usize::from(inner.height / CARD_ROWS),
    );

    let first = rows.start * columns + 1;
    let last = (rows.end * columns).min(members.len());
    let mut counter = format!(" {first}–{last} / {} ", members.len());
    if rows.start > 0 {
        counter.insert(1, '▴');
    }
    if rows.end < total_rows {
        counter.push('▾');
    }
    let block = block.title(Line::styled(counter, Style::new().fg(theme.muted)).right_aligned());
    frame.render_widget(block, area);

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
    let border = Style::new().fg(if card.current {
        theme.accent
    } else {
        theme.muted
    });
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border);
    if card.cursor {
        block = block.style(theme.surface);
    }
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let width = usize::from(inner.width);

    let mark = if card.current { "✓" } else { " " };
    let name = truncate(card.name, width.saturating_sub(2));
    let padding = width.saturating_sub(name.width() + 1);
    let title = Line::from(vec![
        Span::styled(
            name,
            Style::new().fg(theme.fg).add_modifier(if card.current {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
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
        Some(0) => (
            "超时".to_owned(),
            Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
        ),
        Some(ms) => (
            format!("{ms} ms"),
            Style::new()
                .fg(theme.delay(ms))
                .add_modifier(Modifier::BOLD),
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
        Some(ms) if ms > 0 => {
            (usize::try_from(ms).unwrap_or(BAR_FULL_MS).min(BAR_FULL_MS) * bar_width / BAR_FULL_MS)
                .max(1)
        }
        _ => 0,
    };
    let bar_color = delay.map_or(theme.muted, |ms| theme.delay(ms));
    let bottom = Line::from(vec![
        Span::styled("╰", border),
        Span::styled("━".repeat(filled), Style::new().fg(bar_color)),
        Span::styled("─".repeat(bar_width - filled.min(bar_width)), border),
        Span::styled("╯", border),
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
