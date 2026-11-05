use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::{App, Reconnect},
    ui::theme::Theme,
};

/// One-row notice at the top of the content area while the controller is
/// unreachable: which retry is under way and when the next one fires.
pub fn render(frame: &mut Frame, area: Rect, app: &App, reconnect: &Reconnect) {
    let line = line(
        app.theme,
        reconnect,
        Instant::now(),
        usize::from(area.width),
    );
    frame.render_widget(Paragraph::new(line), area);
}

/// The banner `width` columns wide at `now`. The hint to switch profiles
/// comes last and is the first to go when the row is too narrow, so the
/// state of the reconnect is always there to read.
fn line(theme: Theme, reconnect: &Reconnect, now: Instant, width: usize) -> Line<'static> {
    let bold = Modifier::BOLD;
    let key = Style::new().fg(theme.accent).add_modifier(bold);
    let muted = Style::new().fg(theme.muted);

    let mut line = if reconnect.unauthorized {
        Line::styled(
            "✗ Secret 错误，请用 --secret 或修改配置",
            Style::new().fg(theme.error).add_modifier(bold),
        )
    } else {
        let attempt = reconnect.attempt;
        let status = match reconnect.retry_at {
            Some(at) => {
                let wait = at.saturating_duration_since(now);
                format!(
                    "⚠ 正在重连（第 {attempt} 次，{}s 后）",
                    wait.as_millis().div_ceil(1000)
                )
            }
            None => format!("⚠ 正在重连（第 {attempt} 次）"),
        };
        Line::from(vec![
            Span::styled(status, Style::new().fg(theme.warning).add_modifier(bold)),
            Span::styled("  按 ", muted),
            Span::styled("r", key),
            Span::styled(" 立即重试", muted),
        ])
    };
    let hint = [
        Span::styled(" · ", muted),
        Span::styled("P", key),
        Span::styled(" 切换配置", muted),
    ];
    let hint_width: usize = hint.iter().map(Span::width).sum();
    if line.width() + hint_width <= width {
        line.spans.extend(hint);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;

    fn text(reconnect: &Reconnect, width: usize) -> String {
        let now = Instant::now();
        line(ThemeName::Terminal.into(), reconnect, now, width).to_string()
    }

    #[test]
    fn the_profile_hint_gives_way_before_the_reconnect_state() {
        let retrying = Reconnect {
            attempt: 3,
            retry_at: None,
            unauthorized: false,
        };
        let full = "⚠ 正在重连（第 3 次）  按 r 立即重试 · P 切换配置";
        assert_eq!(text(&retrying, 80), full);
        // One column short: the hint goes whole, the rest stays whole.
        let short = unicode_width::UnicodeWidthStr::width(full) - 1;
        assert_eq!(
            text(&retrying, short),
            "⚠ 正在重连（第 3 次）  按 r 立即重试"
        );
        let rejected = Reconnect {
            unauthorized: true,
            ..retrying
        };
        assert_eq!(
            text(&rejected, 80),
            "✗ Secret 错误，请用 --secret 或修改配置 · P 切换配置"
        );
    }
}
