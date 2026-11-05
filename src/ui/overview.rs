use std::collections::VecDeque;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, Padding, Paragraph},
};

use crate::{
    api::models::{Configs, Mode, Traffic},
    app::{App, TRAFFIC_HISTORY},
    format,
    ui::theme::Theme,
};

/// Rows of the mode panel: two borders and a three-row pill.
const MODE_PANEL_HEIGHT: u16 = 5;

/// Draws the overview: the traffic chart on the left, the mode switch on the
/// right.
pub fn render(frame: &mut Frame, area: Rect, app: &App, configs: &Configs) {
    let [chart, side] = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)]).areas(area);
    render_traffic(frame, chart, app);
    let [mode, _] =
        Layout::vertical([Constraint::Length(MODE_PANEL_HEIGHT), Constraint::Fill(1)]).areas(side);
    render_mode(frame, mode, app.theme, configs);
}

fn panel(theme: Theme, title: Line<'static>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.muted))
        .title(title)
}

fn title(theme: Theme, name: &'static str, hint: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {name}"),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{hint} "), Style::new().fg(theme.muted)),
    ])
}

fn render_traffic(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let legend = Line::from(vec![
        Span::styled("● 上传", Style::new().fg(theme.c1)),
        Span::raw("  "),
        Span::styled("● 下载", Style::new().fg(theme.c2)),
        Span::raw(" "),
    ])
    .right_aligned();
    let block = panel(theme, title(theme, "实时流量", " · 最近 60 秒")).title(legend);

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
    let block = panel(theme, title(theme, "代理模式", "  m 切换")).padding(Padding::horizontal(1));
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

#[cfg(test)]
#[allow(clippy::cast_precision_loss)]
mod tests {
    use super::*;

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
}
