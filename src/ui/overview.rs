use std::collections::VecDeque;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Chart, Dataset, GraphType, Padding, Paragraph, RenderDirection,
        Sparkline,
    },
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models::{Configs, ConnectionStats, CoreKind, Mode, Traffic},
    app::{App, TRAFFIC_HISTORY},
    format,
    ui::{components::title::titled, theme::Theme},
};

/// Rows of the mode panel: two borders and a three-row pill.
const MODE_PANEL_HEIGHT: u16 = 5;

/// Rows of a stat card: two borders, then the label, the value, the subtitle
/// and a two-row sparkline.
const CARD_HEIGHT: u16 = 7;

/// The terminal height from which the stat cards are drawn (plan §7.3).
const CARDS_MIN_HEIGHT: u16 = 28;

/// Placeholder for a value that has not arrived.
const NO_VALUE: &str = "—";

/// The narrowest the right column gets: three bordered mode pills that
/// still hold `全局`, and the kernel panel's longest row
/// (`版本 mihomo v1.19.33`), each with the panel's borders and padding.
const SIDE_MIN_WIDTH: u16 = 24;

/// Draws the overview: the traffic chart on the left, the mode switch and
/// the kernel info on the right, the stat cards along the bottom.
pub fn render(frame: &mut Frame, area: Rect, app: &App, configs: &Configs) {
    let (main, cards) = if app.size.height >= CARDS_MIN_HEIGHT {
        let [main, cards] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(CARD_HEIGHT)]).areas(area);
        (main, Some(cards))
    } else {
        (area, None)
    };
    // A third of the width, but no less than the pills and the kernel
    // rows need: below 72 columns the chart gives up the difference.
    let columns = if main.width / 3 >= SIDE_MIN_WIDTH {
        [Constraint::Fill(2), Constraint::Fill(1)]
    } else {
        [Constraint::Fill(1), Constraint::Length(SIDE_MIN_WIDTH)]
    };
    let [chart, side] = Layout::horizontal(columns).areas(main);
    render_traffic(frame, chart, app);
    let [mode, kernel] =
        Layout::vertical([Constraint::Length(MODE_PANEL_HEIGHT), Constraint::Fill(1)]).areas(side);
    render_mode(frame, mode, app.theme, configs);
    render_kernel(frame, kernel, app, configs);
    if let Some(cards) = cards {
        render_cards(frame, cards, app);
    }
}

/// A muted panel `width` columns wide, titled `left` and, on the right,
/// `right`, as much of them as fits.
fn panel(
    theme: Theme,
    width: u16,
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
) -> Block<'static> {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.muted));
    titled(block, width, left, right)
}

fn title(theme: Theme, name: &'static str, hint: &'static str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!(" {name}"),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{hint} "), Style::new().fg(theme.muted)),
    ]
}

fn render_traffic(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let legend = vec![
        Span::styled("● 上传", Style::new().fg(theme.c1)),
        Span::raw("  "),
        Span::styled("● 下载", Style::new().fg(theme.c2)),
        Span::raw(" "),
    ];
    let block = panel(
        theme,
        area.width,
        title(theme, "实时流量", " · 最近 60 秒"),
        legend,
    );

    let history = &app.traffic_history;
    if history.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        frame.render_widget(
            Paragraph::new(Line::styled("等待流量数据…", Style::new().fg(theme.muted)))
                .alignment(Alignment::Center),
            middle,
        );
        return;
    }

    let peak = history
        .iter()
        .map(|traffic| traffic.up.max(traffic.down))
        .max()
        .unwrap_or(0);
    let y = y_axis(peak);
    let up = series(history, |traffic| traffic.up);
    let down = series(history, |traffic| traffic.down);
    let line = |color, data| {
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(color))
            .data(data)
    };
    let muted = Style::new().fg(theme.muted);
    #[allow(clippy::cast_precision_loss)]
    let last_slot = (TRAFFIC_HISTORY - 1) as f64;
    let chart = Chart::new(vec![line(theme.c2, &down), line(theme.c1, &up)])
        .block(block)
        .x_axis(Axis::default().bounds([0.0, last_slot]).style(muted))
        .y_axis(
            Axis::default()
                .bounds([0.0, y.bound])
                .labels(y.labels.map(|label| Span::styled(label, muted)))
                .labels_alignment(Alignment::Right)
                .style(muted),
        );
    frame.render_widget(chart, area);
}

/// Chart points of one direction. The chart has [`TRAFFIC_HISTORY`] slots and
/// the newest sample takes the last one, so a short history fills from the
/// right.
#[allow(clippy::cast_precision_loss)]
fn series(history: &VecDeque<Traffic>, value: fn(&Traffic) -> u64) -> Vec<(f64, f64)> {
    let first = TRAFFIC_HISTORY.saturating_sub(history.len());
    history
        .iter()
        .enumerate()
        .map(|(index, traffic)| ((first + index) as f64, value(traffic) as f64))
        .collect()
}

/// The vertical axis of the traffic chart: four equal steps of a whole number
/// of one unit each.
#[derive(Debug, PartialEq)]
struct YAxis {
    /// Value of the top tick, in bytes per second; never below the peak.
    bound: f64,
    /// Labels of the ticks from the bottom (`0`) to the top (`12 MB`).
    labels: [String; 5],
}

/// Scales the axis to `peak` bytes per second. The unit is the largest one
/// the peak reaches, a step is a quarter of the peak in that unit rounded up
/// to a whole number (at least 1), so a zero peak still gets a range.
fn y_axis(peak: u64) -> YAxis {
    let (size, unit) = format::unit_for(peak);
    let step = peak.div_ceil(4 * size).max(1);
    #[allow(clippy::cast_precision_loss)]
    let bound = (4 * step) as f64 * size as f64;
    let labels = std::array::from_fn(|tick| match step * tick as u64 {
        0 => "0".to_owned(),
        value => format!("{value} {unit}"),
    });
    YAxis { bound, labels }
}

fn render_mode(frame: &mut Frame, area: Rect, theme: Theme, configs: &Configs) {
    let block = panel(
        theme,
        area.width,
        title(theme, "代理模式", "  m 切换"),
        vec![],
    )
    .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let current = configs.mode();
    let slots = Layout::horizontal([Constraint::Fill(1); 3])
        .spacing(1)
        .split(inner);
    for (mode, slot) in Mode::ALL.into_iter().zip(slots.iter()) {
        render_pill(frame, *slot, theme, mode, current == Some(mode));
    }
}

/// One mode as a three-row pill: filled in accent when it is the current one,
/// outlined in muted otherwise.
fn render_pill(frame: &mut Frame, area: Rect, theme: Theme, mode: Mode, current: bool) {
    let (style, label_area) = if current {
        // Reversed, so the fill is accent in any palette.
        let style = Style::new()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        frame.render_widget(Block::new().style(style), area);
        (style, area)
    } else {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme.muted));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        (Style::new().fg(theme.muted), inner)
    };
    let row = Rect {
        y: label_area.y + label_area.height / 2,
        height: label_area.height.min(1),
        ..label_area
    };
    frame.render_widget(
        Paragraph::new(mode.label())
            .style(style)
            .alignment(Alignment::Center),
        row,
    );
}

/// The kernel panel: version and the settings people look up most.
fn render_kernel(frame: &mut Frame, area: Rect, app: &App, configs: &Configs) {
    let theme = app.theme;
    let block =
        panel(theme, area.width, title(theme, "内核", ""), vec![]).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let version = match &app.version {
        Some(Ok(version)) => {
            let core = match version.kind() {
                CoreKind::Mihomo => "mihomo",
                CoreKind::Clash => "Clash",
            };
            (format!("{core} {}", version.version), theme.fg)
        }
        _ => (NO_VALUE.to_owned(), theme.muted),
    };
    let port = match configs.mixed_port {
        None => (NO_VALUE.to_owned(), theme.muted),
        Some(0) => ("未启用".to_owned(), theme.muted),
        Some(port) => (port.to_string(), theme.fg),
    };
    let tun = match &configs.tun {
        Some(tun) => switch(theme, tun.enable),
        None => ("需要 mihomo".to_owned(), theme.muted),
    };
    let lan = configs.allow_lan.map_or_else(
        || (NO_VALUE.to_owned(), theme.muted),
        |on| switch(theme, on),
    );
    let rows = [
        ("版本", version),
        ("混合端口", port),
        ("TUN", tun),
        ("局域网连接", lan),
    ];
    let width = usize::from(inner.width);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(name, (value, color))| kernel_row(theme, width, name, &value, color))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn switch(theme: Theme, on: bool) -> (String, Color) {
    if on {
        ("● 开启".to_owned(), theme.success)
    } else {
        ("○ 关闭".to_owned(), theme.muted)
    }
}

/// A name on the left and its value pushed against the right edge. A value
/// too long for what the name leaves is cut.
fn kernel_row(theme: Theme, width: usize, name: &str, value: &str, color: Color) -> Line<'static> {
    let value = format::truncate(value, width.saturating_sub(name.width() + 1));
    let gap = width.saturating_sub(name.width() + value.width());
    Line::from(vec![
        Span::styled(name.to_owned(), Style::new().fg(theme.muted)),
        Span::raw(" ".repeat(gap)),
        Span::styled(value, Style::new().fg(color)),
    ])
}

/// What the main value of a stat card shows.
enum Reading {
    Bytes(u64),
    Count(u64),
    /// Nothing to show yet: a muted placeholder, and no sparkline.
    Missing(&'static str),
}

struct Card<I> {
    label: &'static str,
    color: Color,
    reading: Reading,
    subtitle: String,
    /// The sparkline's samples, oldest first.
    samples: I,
}

/// Four equally wide cards: totals up and down, open connections, memory.
fn render_cards(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let width = area.width.saturating_sub(3) / 4;
    // Equal widths and one-column gaps; the up to three columns left over
    // stay empty on the right.
    let [upload, download, connections, memory] =
        Layout::horizontal([Constraint::Length(width); 4])
            .spacing(1)
            .flex(Flex::Start)
            .areas(area);

    let stats = app.connections;
    let total = |pick: fn(&ConnectionStats) -> u64| {
        stats.map_or(Reading::Missing(NO_VALUE), |stats| {
            Reading::Bytes(pick(&stats))
        })
    };
    let since_start = "自内核启动";
    render_card(
        frame,
        upload,
        theme,
        Card {
            label: "上传总量",
            color: theme.c1,
            reading: total(|stats| stats.upload_total),
            subtitle: since_start.to_owned(),
            samples: app.traffic_history.iter().map(|traffic| traffic.up),
        },
    );
    render_card(
        frame,
        download,
        theme,
        Card {
            label: "下载总量",
            color: theme.c2,
            reading: total(|stats| stats.download_total),
            subtitle: since_start.to_owned(),
            samples: app.traffic_history.iter().map(|traffic| traffic.down),
        },
    );
    render_card(
        frame,
        connections,
        theme,
        Card {
            label: "活动连接",
            color: theme.c3,
            reading: stats.map_or(Reading::Missing(NO_VALUE), |stats| {
                Reading::Count(stats.active)
            }),
            subtitle: stats.map_or_else(String::new, |stats| {
                format!("TCP {} · UDP {}", stats.tcp, stats.udp)
            }),
            samples: app.connection_history.iter().copied(),
        },
    );
    let clash = matches!(&app.version, Some(Ok(version)) if version.kind() == CoreKind::Clash);
    let reading = if clash {
        Reading::Missing("需要 mihomo")
    } else {
        app.memory
            .map_or(Reading::Missing(NO_VALUE), Reading::Bytes)
    };
    render_card(
        frame,
        memory,
        theme,
        Card {
            label: "内存占用",
            color: theme.c4,
            reading,
            subtitle: "mihomo 进程".to_owned(),
            samples: app.memory_history.iter().copied(),
        },
    );
}

fn render_card<I>(frame: &mut Frame, area: Rect, theme: Theme, card: Card<I>)
where
    I: DoubleEndedIterator<Item = u64>,
{
    let block = panel(theme, area.width, vec![], vec![]).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [label, value, subtitle, spark] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    let muted = Style::new().fg(theme.muted);
    let fit = |text: &str| format::truncate(text, usize::from(inner.width));

    frame.render_widget(Paragraph::new(Span::styled(fit(card.label), muted)), label);
    let bold = Style::new().fg(card.color).add_modifier(Modifier::BOLD);
    let value_line = match card.reading {
        Reading::Bytes(count) => {
            let (number, unit) = format::bytes(count);
            Line::from(vec![
                Span::styled(number, bold),
                Span::raw(" "),
                Span::styled(unit, muted),
            ])
        }
        Reading::Count(count) => Line::styled(count.to_string(), bold),
        Reading::Missing(text) => Line::styled(fit(text), muted),
    };
    let has_value = !matches!(card.reading, Reading::Missing(_));
    frame.render_widget(Paragraph::new(value_line), value);
    frame.render_widget(
        Paragraph::new(Span::styled(fit(&card.subtitle), muted)),
        subtitle,
    );
    if has_value {
        render_sparkline(frame, spark, card.color, card.samples);
    }
}

/// Newest sample in the last column, older ones to its left; a history
/// shorter than the width leaves the left side empty, as the traffic chart
/// does. Only the newest `area.width` samples can be drawn.
fn render_sparkline(
    frame: &mut Frame,
    area: Rect,
    color: Color,
    samples: impl DoubleEndedIterator<Item = u64>,
) {
    let newest_first = samples.rev().take(usize::from(area.width));
    let sparkline = Sparkline::default()
        .direction(RenderDirection::RightToLeft)
        .style(Style::new().fg(color))
        .data(newest_first);
    frame.render_widget(sparkline, area);
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::config::ThemeName;

    const MB: u64 = 1 << 20;

    fn labels(peak: u64) -> Vec<String> {
        y_axis(peak).labels.to_vec()
    }

    #[test]
    fn idle_traffic_still_has_a_range() {
        let axis = y_axis(0);
        assert_eq!(labels(0), ["0", "1 B", "2 B", "3 B", "4 B"]);
        assert!(axis.bound > 0.0);
    }

    #[test]
    fn a_peak_on_a_tick_is_not_rounded_further() {
        assert_eq!(labels(12 * MB), ["0", "3 MB", "6 MB", "9 MB", "12 MB"]);
        assert_eq!(y_axis(12 * MB).bound, 12.0 * MB as f64);
    }

    #[test]
    fn a_peak_between_ticks_rounds_the_step_up() {
        // 10 MB / 4 = 2.5: steps of 3 MB, top at 12 MB.
        assert_eq!(labels(10 * MB), ["0", "3 MB", "6 MB", "9 MB", "12 MB"]);
    }

    #[test]
    fn the_unit_changes_at_1024() {
        assert_eq!(labels(1023), ["0", "256 B", "512 B", "768 B", "1024 B"]);
        assert_eq!(labels(1024), ["0", "1 KB", "2 KB", "3 KB", "4 KB"]);
        assert_eq!(
            labels(MB - 1),
            ["0", "256 KB", "512 KB", "768 KB", "1024 KB"]
        );
        assert_eq!(labels(MB), ["0", "1 MB", "2 MB", "3 MB", "4 MB"]);
        assert_eq!(labels(MB + 1), ["0", "1 MB", "2 MB", "3 MB", "4 MB"]);
        assert_eq!(labels(4 * MB + 1), ["0", "2 MB", "4 MB", "6 MB", "8 MB"]);
    }

    #[test]
    fn gigabytes_are_the_last_unit() {
        let gb = 1 << 30;
        assert_eq!(labels(5000 * gb)[4], "5000 GB");
        // The largest peak neither overflows nor shrinks the range.
        let axis = y_axis(u64::MAX);
        assert!(axis.bound >= u64::MAX as f64);
    }

    #[test]
    fn ticks_are_whole_numbers_and_the_top_covers_the_peak() {
        let peaks = (0..5000u64).chain([
            MB - 1,
            MB,
            MB + 1,
            7 * MB + 3,
            1 << 30,
            (1 << 30) + 1,
            u64::MAX / 2,
        ]);
        for peak in peaks {
            let axis = y_axis(peak);
            assert!(axis.bound >= peak as f64, "peak {peak}");
            let values: Vec<u64> = axis
                .labels
                .iter()
                .map(|label| {
                    let number = label.split(' ').next().unwrap();
                    number
                        .parse()
                        .unwrap_or_else(|_| panic!("{label:?} at {peak}"))
                })
                .collect();
            assert_eq!(values[0], 0);
            let step = values[1];
            assert!(step >= 1, "peak {peak}");
            for (tick, value) in values.iter().enumerate() {
                assert_eq!(*value, step * tick as u64, "peak {peak}");
            }
        }
    }

    #[test]
    fn a_short_history_fills_from_the_right() {
        let history: VecDeque<Traffic> = (1..=3).map(|n| Traffic { up: n, down: 0 }).collect();
        let points = series(&history, |traffic| traffic.up);
        assert_eq!(points, [(57.0, 1.0), (58.0, 2.0), (59.0, 3.0)]);
    }

    /// The text of row 0 of a sparkline drawn into `width` columns.
    fn spark(samples: Vec<u64>, width: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| {
                render_sparkline(frame, frame.area(), Color::Reset, samples.into_iter());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..width)
            .map(|x| buffer[(x, 0)].symbol().to_owned())
            .collect()
    }

    #[test]
    fn a_short_sparkline_fills_from_the_right() {
        let cells = spark(vec![4, 8], 5);
        assert_eq!(&cells[..3], [" ", " ", " "]);
        assert_ne!(cells[3], " ");
        assert_eq!(cells[4], "█");
    }

    #[test]
    fn a_long_sparkline_keeps_the_newest_samples() {
        // Oldest first: the tallest sample is the newest.
        let cells = spark((1..=10).collect(), 3);
        assert_eq!(cells[2], "█");
        assert!(cells[..2].iter().all(|cell| cell != " " && cell != "█"));
    }

    #[test]
    fn a_kernel_row_never_outgrows_its_width() {
        let theme = Theme::from(ThemeName::Terminal);
        for value in [
            "7890",
            "● 开启",
            "需要 mihomo",
            "mihomo v1.19.33-with-a-long-suffix",
        ] {
            for width in 0..40 {
                let line = kernel_row(theme, width, "局域网连接", value, theme.fg);
                assert!(line.width() <= width.max(10), "{value:?} in {width}");
            }
        }
        let line = kernel_row(theme, 20, "TUN", "● 开启", theme.fg);
        assert_eq!(line.width(), 20);
    }
}
