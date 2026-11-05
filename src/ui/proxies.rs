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
    app::{App, Pane},
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
    render_nodes(frame, right, app, groups[selected]);
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

fn render_nodes(frame: &mut Frame, area: Rect, app: &App, group: &Proxy) {
    let theme = app.theme;
    let focused = app.proxies_view.focus == Pane::Nodes;
    let count = group.all.as_ref().map_or(0, Vec::len);
    let title = Line::from(vec![
        Span::styled(
            format!(" {}", group.name),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {} · {count} 个节点 ", group.kind),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(panel(app, focused, title), area);
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
