use jiff::{Timestamp, tz::TimeZone};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Paragraph, Row, Table, TableState},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, ConnectionRow, ConnectionSort, Detail, SortColumn},
    format,
    ui::{
        COMPACT_WIDTH,
        components::{popup, states, title},
        hit::Target,
        text::wrap,
    },
};

/// Widest rate: `1023.9 KB/s`.
const RATE_WIDTH: u16 = 11;
/// Widest size: `1023.9 KB`.
const TOTAL_WIDTH: u16 = 9;
/// Widest duration below 100 days: `12m05s`, `99d23h`. Also exactly
/// `时长 ▲`.
const DURATION_WIDTH: u16 = 6;
/// Columns between the cells.
const SPACING: u16 = 1;

const NO_VALUE: &str = "—";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Host,
    Process,
    Route,
    Up,
    Down,
    Total,
    Duration,
}

impl Column {
    const ALL: [Self; 7] = [
        Self::Host,
        Self::Process,
        Self::Route,
        Self::Up,
        Self::Down,
        Self::Total,
        Self::Duration,
    ];

    /// What a narrow terminal keeps.
    const NARROW: [Self; 5] = [
        Self::Host,
        Self::Route,
        Self::Up,
        Self::Down,
        Self::Duration,
    ];

    /// The text columns share what the numbers leave, host and route taking
    /// more than the process.
    fn constraint(self) -> Constraint {
        match self {
            Self::Host => Constraint::Fill(3),
            Self::Process => Constraint::Fill(2),
            Self::Route => Constraint::Fill(4),
            Self::Up | Self::Down => Constraint::Length(RATE_WIDTH),
            Self::Total => Constraint::Length(TOTAL_WIDTH),
            Self::Duration => Constraint::Length(DURATION_WIDTH),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Host => "主机",
            Self::Process => "进程",
            Self::Route => "规则 → 链路",
            Self::Up => "上行",
            Self::Down => "下行",
            Self::Total => "总量",
            Self::Duration => "时长",
        }
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::Up | Self::Down | Self::Total | Self::Duration)
    }

    /// The order this column stands for; `None` for the route, which does
    /// not sort.
    fn sort_column(self) -> Option<SortColumn> {
        match self {
            Self::Host => Some(SortColumn::Host),
            Self::Process => Some(SortColumn::Process),
            Self::Route => None,
            Self::Up => Some(SortColumn::Up),
            Self::Down => Some(SortColumn::Down),
            Self::Total => Some(SortColumn::Total),
            Self::Duration => Some(SortColumn::Duration),
        }
    }
}

/// The columns a terminal of `size` shows.
fn shown(size: Size) -> &'static [Column] {
    if size.width >= COMPACT_WIDTH {
        &Column::ALL
    } else {
        &Column::NARROW
    }
}

/// The columns `s` steps through in a terminal of `size`, left to right.
pub fn sort_columns(size: Size) -> impl Iterator<Item = SortColumn> {
    shown(size).iter().filter_map(|column| column.sort_column())
}

fn panel_inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

/// Table rows that fit in a page body `body`: the panel's inside less the
/// header.
pub fn capacity(body: Rect) -> usize {
    usize::from(panel_inner(body).height.saturating_sub(1))
}

/// The columns shown in a table `inner` wide, with their widths.
fn columns(inner: Rect, shown: &[Column]) -> Vec<(Column, usize)> {
    let widths = Layout::horizontal(shown.iter().map(|column| column.constraint()))
        .spacing(SPACING)
        .split(inner);
    shown
        .iter()
        .zip(widths.iter())
        .map(|(&column, rect)| (column, usize::from(rect.width)))
        .collect()
}

/// What the mouse can act on in the table inside `inner`: each of the rows
/// of `window`, and the headings that sort. The route does not sort.
fn register_hits(
    app: &App,
    inner: Rect,
    columns: &[(Column, usize)],
    visible: &[&ConnectionRow],
    window: std::ops::Range<usize>,
) {
    // The header is the table's first row.
    for (offset, index) in window.enumerate() {
        let y = inner.y + 1 + u16::try_from(offset).unwrap_or(0);
        app.hits.add(
            Rect::new(inner.x, y, inner.width, 1),
            Target::Connection(visible[index].connection.id.clone()),
        );
    }
    let mut x = inner.x;
    for &(column, width) in columns {
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        if let Some(sort) = column.sort_column() {
            app.hits
                .add(Rect::new(x, inner.y, width, 1), Target::Column(sort));
        }
        x = x.saturating_add(width).saturating_add(SPACING);
    }
}

/// The connections page: one panel holding the live table.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let view = &app.connections_view;
    let Some(rows) = &view.rows else {
        states::loading(frame, area, app, "等待连接数据…");
        return;
    };
    let visible = view.visible();
    let filter = view.filter.value();

    let status = if view.paused {
        Span::styled(
            format!(" {} 已暂停 ", app.icons.paused),
            Style::new().fg(theme.warning),
        )
    } else {
        Span::styled(
            format!(" {} 实时 ", app.icons.dot),
            Style::new().fg(theme.success),
        )
    };
    let count = if filter.is_empty() {
        format!("  {} ", rows.len())
    } else {
        format!("  {}/{} · 过滤「{filter}」 ", visible.len(), rows.len())
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent));
    let block = title::titled(
        block,
        area.width,
        vec![
            Span::styled(
                " 连接",
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(count, Style::new().fg(theme.muted)),
        ],
        vec![status],
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hits.add(inner, Target::List);

    if visible.is_empty() {
        if filter.is_empty() {
            states::empty(frame, inner, app, "当前没有活动连接", &[]);
        } else {
            states::empty(
                frame,
                inner,
                app,
                &format!("没有匹配「{filter}」的连接"),
                &[("Esc", "清除过滤"), ("/", "修改过滤")],
            );
        }
        return;
    }

    let columns = columns(inner, shown(app.size));
    let cursor = view.cursor.min(visible.len() - 1);
    let window = view.scroll.window(cursor, visible.len(), capacity(area));
    register_hits(app, inner, &columns, &visible, window.clone());

    let header = Row::new(
        columns
            .iter()
            .map(|&(column, width)| Cell::from(heading(app, view.sort, column, width))),
    );
    // Only the rows in view are built: the kernel can have thousands open.
    let start = window.start;
    let body = window.map(|index| {
        let style = if index % 2 == 1 {
            theme.stripe
        } else {
            Style::new()
        };
        let cells = columns.iter().map(|&(column, width)| {
            Cell::from(cell(app, visible[index], view.as_of, column, width))
        });
        Row::new(cells).style(style)
    });
    let widths = columns
        .iter()
        .map(|&(_, width)| Constraint::Length(u16::try_from(width).unwrap_or(u16::MAX)));
    // The highlight goes on after the cells, as `Theme::paint_cursor` does.
    frame.render_stateful_widget(
        Table::new(body, widths)
            .header(header)
            .column_spacing(SPACING)
            .row_highlight_style(theme.surface),
        inner,
        &mut TableState::new().with_selected(cursor - start),
    );
}

/// A column's header: muted, or accent with `▲` / `▼` at the end when the
/// table is sorted by it. The arrow is kept when the label has to give.
fn heading(app: &App, sort: ConnectionSort, column: Column, width: usize) -> Line<'static> {
    let theme = app.theme;
    let sorted = column.sort_column() == Some(sort.column);
    let text = if sorted {
        let arrow = if sort.ascending { '▲' } else { '▼' };
        let spaced = format!("{} {arrow}", column.label());
        if spaced.width() <= width {
            spaced
        } else {
            format!(
                "{}{arrow}",
                format::truncate(column.label(), width.saturating_sub(1))
            )
        }
    } else {
        format::truncate(column.label(), width)
    };
    let color = if sorted { theme.accent } else { theme.muted };
    let used = text.width();
    let span = Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD));
    aligned(vec![span], used, width, column.right_aligned())
}

/// `spans`, `used` columns wide, padded to `width`.
fn aligned(mut spans: Vec<Span<'static>>, used: usize, width: usize, right: bool) -> Line<'static> {
    if right {
        spans.insert(0, Span::raw(" ".repeat(width.saturating_sub(used))));
    }
    Line::from(spans)
}

fn cell(
    app: &App,
    row: &ConnectionRow,
    as_of: Option<Timestamp>,
    column: Column,
    width: usize,
) -> Line<'static> {
    let theme = app.theme;
    let connection = &row.connection;
    let muted = Style::new().fg(theme.muted);
    let plain = Style::new().fg(theme.fg);
    let text = |text: &str, style: Style| {
        let text = format::truncate(text, width);
        let used = text.width();
        (vec![Span::styled(text, style)], used)
    };
    let (spans, used) = match column {
        Column::Host => match row.host() {
            "" => text(NO_VALUE, muted),
            // A connection that just opened stands out for a few frames.
            name if row.fresh => text(
                name,
                Style::new().fg(theme.success).add_modifier(Modifier::BOLD),
            ),
            name => text(name, plain),
        },
        Column::Process => match connection.metadata.process.as_str() {
            "" => text(NO_VALUE, muted),
            process => text(process, plain),
        },
        Column::Route => route(app, row, width),
        Column::Up => rate(app, row.rate.map(|rate| rate.up), theme.c1),
        Column::Down => rate(app, row.rate.map(|rate| rate.down), theme.c2),
        Column::Total => {
            let (number, unit) =
                format::bytes(connection.upload.saturating_add(connection.download));
            let used = number.width() + 1 + unit.width();
            (
                vec![
                    Span::styled(number, plain),
                    Span::styled(format!(" {unit}"), muted),
                ],
                used,
            )
        }
        Column::Duration => match elapsed(connection.start, as_of) {
            Some(secs) => text(&format::duration(secs), plain),
            None => text(NO_VALUE, muted),
        },
    };
    aligned(spans, used, width, column.right_aligned())
}

/// The route from the outermost group to the node, which is how the kernel
/// lists it back to front; empty when it gave none.
fn chain(row: &ConnectionRow) -> String {
    row.connection
        .chains
        .iter()
        .rev()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" → ")
}

/// `RuleSet → Proxy → Auto → Node 1`: the rule type in muted, then the
/// route.
fn route(app: &App, row: &ConnectionRow, width: usize) -> (Vec<Span<'static>>, usize) {
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    let chain = chain(row);
    let chain = if chain.is_empty() {
        NO_VALUE.to_owned()
    } else {
        chain
    };
    let rule = if row.connection.rule.is_empty() {
        String::new()
    } else {
        format!("{} → ", row.connection.rule)
    };

    let rule_width = rule.width();
    if rule_width >= width {
        // Nothing left for the route: the rule is all there is room for.
        let text = format::truncate(&rule, width);
        let used = text.width();
        return (vec![Span::styled(text, muted)], used);
    }
    let chain = format::truncate(&chain, width - rule_width);
    let used = rule_width + chain.width();
    let chain_style = if chain == NO_VALUE {
        muted
    } else {
        Style::new().fg(theme.fg)
    };
    (
        vec![Span::styled(rule, muted), Span::styled(chain, chain_style)],
        used,
    )
}

/// A rate as number (in `color`) and unit (muted). Nothing moved: all muted;
/// no earlier frame to compare with: a dash.
fn rate(app: &App, bytes_per_frame: Option<u64>, color: Color) -> (Vec<Span<'static>>, usize) {
    let muted = Style::new().fg(app.theme.muted);
    let Some(bytes) = bytes_per_frame else {
        return (vec![Span::styled(NO_VALUE, muted)], NO_VALUE.width());
    };
    let (number, unit) = format::rate(bytes);
    let used = number.width() + 1 + unit.width();
    let number_style = if bytes == 0 {
        muted
    } else {
        Style::new().fg(color)
    };
    (
        vec![
            Span::styled(number, number_style),
            Span::styled(format!(" {unit}"), muted),
        ],
        used,
    )
}

/// Whole seconds from `start` to `as_of`; `None` when the kernel gave no
/// readable start. A start in the future (clocks apart) counts as 0.
fn elapsed(start: Option<Timestamp>, as_of: Option<Timestamp>) -> Option<u64> {
    let secs = as_of?.duration_since(start?).as_secs();
    Some(u64::try_from(secs).unwrap_or(0))
}

/// Widest the detail popup gets, borders included.
const DETAIL_WIDTH: u16 = 72;

/// Columns of the detail popup's name column: `开始时间` and a gap of two.
const NAME_WIDTH: usize = 10;

/// `ip:port`, with an IPv6 address in brackets (`[2001:db8::1]:443`); just
/// the address when there is no port, nothing when there is no address.
fn address(ip: &str, port: &str) -> String {
    if ip.is_empty() || port.is_empty() {
        ip.to_owned()
    } else if ip.contains(':') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// The connection detail popup: one row per property, names muted on the
/// left, values wrapped to the width that is left.
pub fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some(detail) = &app.connections_view.detail else {
        return;
    };
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    let width = DETAIL_WIDTH.min(area.width);
    // Borders and the panel's padding take two columns a side.
    let value_width = usize::from(width.saturating_sub(4))
        .saturating_sub(NAME_WIDTH)
        .max(1);

    let mut lines = vec![];
    for (name, value) in detail_rows(app, detail) {
        for (index, part) in wrap(value, value_width).into_iter().enumerate() {
            let name = if index == 0 { name } else { "" };
            let padding = " ".repeat(NAME_WIDTH.saturating_sub(name.width()));
            let mut spans = vec![Span::styled(format!("{name}{padding}"), muted)];
            spans.extend(part);
            lines.push(Line::from(spans));
        }
    }

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let rect = popup::modal(app, area, width, height);
    let status = detail
        .closed
        .then(|| Span::styled("已关闭 ", Style::new().fg(theme.error)));
    let inner = popup::panel(frame, rect, theme.accent, Some("连接详情"), status);

    let rows = usize::from(inner.height);
    if lines.len() > rows {
        lines.truncate(rows);
        if let Some(last) = lines.pop() {
            // What fits ahead of the ellipsis: the first line of a wrap that
            // leaves it a column.
            let budget = usize::from(inner.width).saturating_sub(1);
            let mut spans = wrap(last.spans, budget.max(1)).swap_remove(0);
            spans.push(Span::styled("…", muted));
            lines.push(Line::from(spans));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The popup's rows as `(name, value)`; a missing value is a muted dash.
fn detail_rows(app: &App, detail: &Detail) -> Vec<(&'static str, Vec<Span<'static>>)> {
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    let plain = Style::new().fg(theme.fg);
    let text = |value: String| {
        if value.is_empty() {
            vec![Span::styled(NO_VALUE, muted)]
        } else {
            vec![Span::styled(value, plain)]
        }
    };
    let connection = &detail.row.connection;
    let metadata = &connection.metadata;

    let network = [metadata.network.to_uppercase(), metadata.kind.clone()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let process = match (metadata.process.as_str(), metadata.process_path.as_str()) {
        ("", "") => vec![Span::styled(NO_VALUE, muted)],
        (process, "") => vec![Span::styled(process.to_owned(), plain)],
        ("", path) => vec![Span::styled(path.to_owned(), muted)],
        (process, path) => vec![
            Span::styled(process.to_owned(), plain),
            Span::styled(format!(" {path}"), muted),
        ],
    };
    let rule = match (connection.rule.as_str(), connection.rule_payload.as_str()) {
        (rule, "") => rule.to_owned(),
        (rule, payload) => format!("{rule}({payload})"),
    };
    let start = connection.start.map_or_else(String::new, |start| {
        start
            .to_zoned(TimeZone::system())
            .strftime("%Y-%m-%d %H:%M:%S")
            .to_string()
    });
    let duration =
        elapsed(connection.start, detail.as_of).map_or_else(String::new, format::duration);
    let rate = detail.row.rate;
    // A connection routed by its domain name may not have been resolved:
    // the name is where it goes then, and the port is still worth seeing.
    let destination = if metadata.destination_ip.is_empty() {
        &metadata.host
    } else {
        &metadata.destination_ip
    };

    vec![
        ("主机", text(metadata.host.clone())),
        (
            "目标",
            text(address(destination, &metadata.destination_port)),
        ),
        (
            "来源",
            text(address(&metadata.source_ip, &metadata.source_port)),
        ),
        ("网络", text(network)),
        ("进程", process),
        ("规则", text(rule)),
        ("链路", text(chain(&detail.row))),
        ("开始时间", text(start)),
        ("时长", text(duration)),
        (
            "上传",
            traffic(app, connection.upload, rate.map(|rate| rate.up), theme.c1),
        ),
        (
            "下载",
            traffic(
                app,
                connection.download,
                rate.map(|rate| rate.down),
                theme.c2,
            ),
        ),
    ]
}

/// `5.0 MB · 1.0 MB/s`: what moved so far and the current rate, numbers in
/// `color`.
fn traffic(app: &App, total: u64, rate_now: Option<u64>, color: Color) -> Vec<Span<'static>> {
    let muted = Style::new().fg(app.theme.muted);
    let (number, unit) = format::bytes(total);
    let mut spans = vec![
        Span::styled(number, Style::new().fg(color)),
        Span::styled(format!(" {unit} · "), muted),
    ];
    spans.extend(rate(app, rate_now, color).0);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_bracket_ipv6_and_drop_a_missing_port() {
        assert_eq!(address("203.0.113.10", "443"), "203.0.113.10:443");
        assert_eq!(address("2001:db8::1", "443"), "[2001:db8::1]:443");
        assert_eq!(address("203.0.113.10", ""), "203.0.113.10");
        assert_eq!(address("2001:db8::1", ""), "2001:db8::1");
        assert_eq!(address("", "443"), "");
    }
}
