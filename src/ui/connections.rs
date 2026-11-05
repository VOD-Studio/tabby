use jiff::Timestamp;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Row, Table},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, ConnectionRow},
    format,
    ui::components::states,
};

/// Below this terminal width the process and total columns are left out
/// (plan §7.3).
const NARROW: u16 = 70;

/// Widest rate: `1023.9 KB/s`.
const RATE_WIDTH: u16 = 11;
/// Widest size: `1023.9 KB`.
const TOTAL_WIDTH: u16 = 9;
/// Widest duration below 100 days: `12m05s`, `99d23h`.
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
fn columns(inner: Rect, wide: bool) -> Vec<(Column, usize)> {
    let shown: &[Column] = if wide { &Column::ALL } else { &Column::NARROW };
    let widths = Layout::horizontal(shown.iter().map(|column| column.constraint()))
        .spacing(SPACING)
        .split(inner);
    shown
        .iter()
        .zip(widths.iter())
        .map(|(&column, rect)| (column, usize::from(rect.width)))
        .collect()
}

/// The connections page: one panel holding the live table.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let view = &app.connections_view;
    let Some(rows) = &view.rows else {
        states::loading(frame, area, app, "等待连接数据…");
        return;
    };

    let status = if view.paused {
        Span::styled(" ⏸ 已暂停 ", Style::new().fg(theme.warning))
    } else {
        Span::styled(" ● 实时 ", Style::new().fg(theme.success))
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title(Line::from(vec![
            Span::styled(
                " 连接",
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {} ", rows.len()), Style::new().fg(theme.muted)),
        ]))
        .title(Line::from(status).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if rows.is_empty() {
        states::empty(frame, inner, app, "当前没有活动连接", &[]);
        return;
    }

    let columns = columns(inner, app.size.width >= NARROW);
    let cursor = view.cursor.min(rows.len() - 1);
    let window = view.scroll.window(cursor, rows.len(), capacity(area));

    let header = Row::new(columns.iter().map(|&(column, width)| {
        let text = format::truncate(column.label(), width);
        let span = Span::styled(
            text.clone(),
            Style::new().fg(theme.muted).add_modifier(Modifier::BOLD),
        );
        Cell::from(aligned(
            vec![span],
            text.width(),
            width,
            column.right_aligned(),
        ))
    }));
    // Only the rows in view are built: the kernel can have thousands open.
    let body = window.map(|index| {
        let style = if index == cursor {
            theme.surface
        } else if index % 2 == 1 {
            theme.stripe
        } else {
            Style::new()
        };
        let cells = columns
            .iter()
            .map(|&(column, width)| Cell::from(cell(app, &rows[index], view.as_of, column, width)));
        Row::new(cells).style(style)
    });
    let widths = columns
        .iter()
        .map(|&(_, width)| Constraint::Length(u16::try_from(width).unwrap_or(u16::MAX)));
    frame.render_widget(
        Table::new(body, widths)
            .header(header)
            .column_spacing(SPACING),
        inner,
    );
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
        Column::Host => {
            let metadata = &connection.metadata;
            let name = [&metadata.host, &metadata.destination_ip]
                .into_iter()
                .find(|name| !name.is_empty());
            match name {
                // A connection that just opened stands out for a few frames.
                Some(name) if row.fresh => text(
                    name,
                    Style::new().fg(theme.success).add_modifier(Modifier::BOLD),
                ),
                Some(name) => text(name, plain),
                None => text(NO_VALUE, muted),
            }
        }
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

/// `RuleSet → Proxy → Auto → Node 1`: the rule type in muted, then the route
/// from the outermost group to the node, which is how the kernel lists it
/// back to front.
fn route(app: &App, row: &ConnectionRow, width: usize) -> (Vec<Span<'static>>, usize) {
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    let chain = row
        .connection
        .chains
        .iter()
        .rev()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" → ");
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
