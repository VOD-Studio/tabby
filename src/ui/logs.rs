use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models::{LogKind, LogLevel},
    app::{App, LogEntry, LogLink, LogsView},
    format,
    ui::{components::states, text::wrap, theme::Theme},
};

/// `HH:MM:SS`.
const TIME_WIDTH: usize = 8;
/// `DEBUG`, `ERROR`: the widest label.
const LABEL_WIDTH: usize = 5;
/// Columns before the message: time, a space, the label, a space. Wrapped
/// lines of a message start here too.
pub const GUTTER: usize = TIME_WIDTH + 1 + LABEL_WIDTH + 1;

/// The room for log lines in a page body: rows, and columns of the message
/// column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub width: usize,
    pub height: usize,
}

/// What fits in a page body `body`, laid out as [`render`] draws it.
pub fn fit(body: Rect) -> Fit {
    let inner = Block::bordered().inner(body);
    Fit {
        width: usize::from(inner.width).saturating_sub(GUTTER).max(1),
        height: usize::from(inner.height),
    }
}

/// The lines `message` takes in a column `width` wide: broken at each `\n`,
/// then by display width. Wide characters are never split; a wrapped line
/// does not start with the space it broke at. Always at least one line.
pub fn wrap_message(message: &str, width: usize) -> Vec<String> {
    message
        .split('\n')
        .flat_map(|line| wrap(vec![Span::raw(line.to_owned())], width))
        .map(|spans| spans.into_iter().map(|span| span.content).collect())
        .collect()
}

/// The label of a level and its color. A level tabby does not know shows
/// as the kernel's word, uppercased, muted.
fn label(kind: &LogKind, theme: &Theme) -> (String, Color) {
    match kind {
        LogKind::Level(level) => (level_label(*level).to_owned(), level_color(*level, theme)),
        LogKind::Other(word) => (
            format::truncate(&word.to_uppercase(), LABEL_WIDTH),
            theme.muted,
        ),
    }
}

fn level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warning => "WARN",
        LogLevel::Error => "ERROR",
    }
}

/// Debug recedes, info reads as fine, warnings and errors stand out.
fn level_color(level: LogLevel, theme: &Theme) -> Color {
    match level {
        LogLevel::Debug => theme.muted,
        LogLevel::Info => theme.success,
        LogLevel::Warning => theme.warning,
        LogLevel::Error => theme.error,
    }
}

/// One screen row: row `row` of `entry`'s wrapped message, `text`. The
/// first row carries the time and the label; the rows after it are indented
/// to line up with the message.
pub fn line(entry: &LogEntry, row: usize, text: String, theme: &Theme) -> Line<'static> {
    let message = Span::styled(text, Style::new().fg(theme.fg));
    if row > 0 {
        return Line::from(vec![Span::raw(" ".repeat(GUTTER)), message]);
    }
    let (label, color) = label(&entry.kind, theme);
    // Padded by display width, not by characters.
    let pad = " ".repeat(LABEL_WIDTH.saturating_sub(label.width()) + 1);
    Line::from(vec![
        Span::styled(
            entry.time.strftime("%H:%M:%S").to_string(),
            Style::new().fg(theme.muted),
        ),
        Span::raw(" "),
        Span::styled(label, Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::raw(pad),
        message,
    ])
}

/// The right side of the title: whether the stream is connected, and else
/// whether the view is paused. Nothing claims live while a resubscription
/// is under way.
pub fn status(view: &LogsView, theme: &Theme) -> Span<'static> {
    match view.link {
        LogLink::Down => Span::styled(" ○ 未连接 ", Style::new().fg(theme.error)),
        LogLink::Resubscribing => Span::styled(" ◌ 连接中 ", Style::new().fg(theme.muted)),
        LogLink::Live if view.paused => Span::styled(
            format!(" ⏸ 已暂停 · 新增 {} 条 ", view.arrived()),
            Style::new().fg(theme.warning),
        ),
        LogLink::Live => Span::styled(" ● 实时 ", Style::new().fg(theme.success)),
    }
}

/// The logs page: one panel with the log, newest at the bottom.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let view = &app.logs_view;
    if view.waiting() {
        states::loading(frame, area, app, "等待日志…");
        return;
    }
    let filter = view.filter.value();
    let shown = view.shown_count();

    let count = if filter.is_empty() {
        format!(" · {} ", view.entries.len())
    } else {
        format!(" · {shown}/{} · 过滤「{filter}」 ", view.entries.len())
    };
    let muted = Style::new().fg(theme.muted);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title(Line::from(vec![
            Span::styled(
                " 日志",
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  级别 ", muted),
            Span::styled(
                level_label(view.level),
                Style::new()
                    .fg(level_color(view.level, &theme))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(count, muted),
        ]))
        .title(Line::from(status(view, &theme)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if shown == 0 {
        if filter.is_empty() {
            states::empty(
                frame,
                inner,
                app,
                "当前级别下暂无日志",
                &[("l", "切换级别")],
            );
        } else {
            states::empty(
                frame,
                inner,
                app,
                &format!("没有匹配「{filter}」的日志"),
                &[("Esc", "清除过滤"), ("/", "修改过滤")],
            );
        }
        return;
    }

    // Only the rows in view are laid out: the buffer holds up to 2000.
    let lines: Vec<Line> = view
        .page(fit(area))
        .into_iter()
        .map(|(entry, row, text)| line(entry, row, text, &theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use jiff::civil::time;

    use super::*;
    use crate::api::models::Log;

    fn columns(line: &Line) -> usize {
        line.spans.iter().map(|span| span.content.width()).sum()
    }

    fn text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn wrapping_never_splits_a_wide_character() {
        let message = "节点选择 🚀 ok 测试";
        // `节点选择` is 8 columns: at 7 the fourth character moves down whole.
        let lines = wrap_message(message, 7);
        assert_eq!(lines, ["节点选", "择 🚀 o", "k 测试"]);
        for width in 2..20 {
            let lines = wrap_message(message, width);
            for line in &lines {
                assert!(line.width() <= width, "{line:?} at {width}");
            }
            // Only the spaces lines broke at are gone.
            let kept: String = lines.concat().chars().filter(|ch| *ch != ' ').collect();
            assert_eq!(kept, message.replace(' ', ""), "at {width}");
        }
    }

    #[test]
    fn a_line_break_in_a_message_starts_a_new_row() {
        assert_eq!(wrap_message("ab\ncdef\n", 3), ["ab", "cde", "f", ""]);
        assert_eq!(wrap_message("", 3), [""]);
    }

    #[test]
    fn wrapped_rows_line_up_with_the_message() {
        let theme = Theme::from(crate::config::ThemeName::Terminal);
        let entry = LogEntry::new(
            Log {
                kind: LogKind::Other("silent".to_owned()),
                payload: "[TCP] 127.0.0.1 --> 例子.example:443 match 节点选择🚀".to_owned(),
            },
            time(9, 5, 7, 0),
        );
        let width = 13;
        let rows = wrap_message(&entry.message, width);
        assert!(rows.len() > 2);
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .map(|(row, text)| line(&entry, row, text, &theme))
            .collect();
        // An unknown level shows as its word, cut to the label's width.
        assert!(text(&lines[0]).starts_with("09:05:07 SILE… [TCP]"));
        for line in &lines[1..] {
            assert!(text(line).starts_with(&" ".repeat(GUTTER)));
            assert!(!text(line)[GUTTER..].starts_with(' '));
        }
        for line in &lines {
            assert!(columns(line) <= GUTTER + width);
        }
    }
}
