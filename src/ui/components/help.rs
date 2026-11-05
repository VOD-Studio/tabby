use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::{App, Page},
    ui::{components::popup, page_title},
};

/// Keys that only work on the given page.
fn page_keys(page: Page) -> &'static [(&'static str, &'static str)] {
    match page {
        Page::Proxies => &[
            ("Enter", "选择节点"),
            ("t", "测当前节点"),
            ("T", "测整组（mihomo）"),
            ("s", "切换排序"),
            ("/", "过滤节点"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动（按行）"),
            ("h / l / ← / →", "同行左右移动"),
            ("h / ←", "最左列回分组"),
            ("l / →", "分组面板进入节点"),
            ("g / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Rules => &[
            ("/", "过滤规则"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动"),
            ("g / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Connections => &[
            ("Enter", "连接详情"),
            ("x", "关闭选中连接"),
            ("X", "关闭全部 / 匹配的"),
            ("s", "切换排序列"),
            ("S", "反转排序"),
            ("/", "过滤连接"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动"),
            ("g / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
            ("p", "暂停 / 恢复刷新"),
        ],
        Page::Logs => &[
            ("l", "切换级别（循环）"),
            ("p", "暂停 / 恢复"),
            ("c", "清空日志"),
            ("/", "过滤日志"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "按行滚动"),
            ("g", "最早"),
            ("G", "最新并跟随"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        _ => &[],
    }
}

/// Keys that work on every page, as `(key, what it does)`.
const GLOBAL_KEYS: [(&str, &str); 6] = [
    ("1-7", "跳到第 N 页"),
    ("Tab / Shift+Tab", "下一页 / 上一页"),
    ("r", "刷新"),
    ("m", "切换代理模式"),
    ("?", "打开 / 关闭帮助"),
    ("q / Ctrl+C", "退出"),
];

const WIDTH: u16 = 74;

/// Centered help overlay: global keys on the left, the current page's on the right.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let page = page_keys(app.page);
    let height = u16::try_from(GLOBAL_KEYS.len().max(page.len()) + 2 + 2).unwrap_or(u16::MAX);
    let rect = popup::centered(area, WIDTH, height);
    let inner = popup::panel(frame, rect, theme.accent, Some("帮助"), None);
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

    let mut page_lines = vec![
        heading(format!("当前页面 · {}", page_title(app.page))),
        Line::raw(""),
    ];
    if page.is_empty() {
        page_lines.push(Line::styled("暂无专属按键", Style::new().fg(theme.muted)));
    }
    page_lines.extend(page.iter().map(|(key, description)| {
        Line::from(vec![
            Span::styled(
                format!("{key:<17}"),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(*description, Style::new().fg(theme.muted)),
        ])
    }));
    frame.render_widget(Paragraph::new(page_lines), right);
}
