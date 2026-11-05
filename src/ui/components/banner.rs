use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Reconnect};

/// One-row notice at the top of the content area while the controller is
/// unreachable: which retry is under way and when the next one fires.
pub fn render(frame: &mut Frame, area: Rect, app: &App, reconnect: &Reconnect) {
    let theme = app.theme;
    let bold = Modifier::BOLD;

    let line = if reconnect.unauthorized {
        Line::styled(
            "✗ Secret 错误，请用 --secret 或修改配置",
            Style::new().fg(theme.error).add_modifier(bold),
        )
    } else {
        let attempt = reconnect.attempt;
        let status = match reconnect.retry_at {
            Some(at) => {
                let wait = at.saturating_duration_since(Instant::now());
                format!(
                    "⚠ 正在重连（第 {attempt} 次，{}s 后）",
                    wait.as_millis().div_ceil(1000)
                )
            }
            None => format!("⚠ 正在重连（第 {attempt} 次）"),
        };
        Line::from(vec![
            Span::styled(status, Style::new().fg(theme.warning).add_modifier(bold)),
            Span::styled("  按 ", Style::new().fg(theme.muted)),
            Span::styled("r", Style::new().fg(theme.accent).add_modifier(bold)),
            Span::styled(" 立即重试", Style::new().fg(theme.muted)),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}
