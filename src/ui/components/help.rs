use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::App,
    ui::{components::popup, page_title},
};

/// Keys that work on every page, as `(key, what it does)`.
const GLOBAL_KEYS: [(&str, &str); 5] = [
    ("1-7", "跳到第 N 页"),
    ("Tab / Shift+Tab", "下一页 / 上一页"),
    ("r", "刷新"),
    ("?", "打开 / 关闭帮助"),
    ("q / Ctrl+C", "退出"),
];

const WIDTH: u16 = 74;

/// Centered help overlay: global keys on the left, the current page's on the right.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let height = u16::try_from(GLOBAL_KEYS.len() + 2 + 2).unwrap_or(u16::MAX);
    let rect = popup::centered(area, WIDTH, height);
    let inner = popup::panel(frame, rect, theme.accent, Some("帮助"));
    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(inner);

    let heading =
        |text: String| Line::styled(text, Style::new().fg(theme.fg).add_modifier(Modifier::BOLD));

    let mut lines = vec![heading("全局".to_owned()), Line::raw("")];
    lines.extend(GLOBAL_KEYS.into_iter().map(|(key, description)| {
        Line::from(vec![
            Span::styled(
                format!("{key:<17}"),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(description, Style::new().fg(theme.muted)),
        ])
    }));
    frame.render_widget(Paragraph::new(lines), left);

    let page = vec![
        heading(format!("当前页面 · {}", page_title(app.page))),
        Line::raw(""),
        Line::styled("暂无专属按键", Style::new().fg(theme.muted)),
    ];
    frame.render_widget(Paragraph::new(page), right);
}
