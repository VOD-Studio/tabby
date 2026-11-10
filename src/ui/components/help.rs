use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, Page},
    ui::{
        COMPACT_WIDTH,
        components::{popup, scrollbar},
        hit::Target,
        page_title, proxies,
    },
};

/// Keys that only work on the given page; the proxies page's depend on
/// whether it shows the group list or the picker.
fn page_keys(page: Page, stacked: bool) -> &'static [(&'static str, &'static str)] {
    match page {
        Page::Proxies if stacked => &[
            ("Enter", "组选择行：展开下拉"),
            ("Enter", "节点：选择节点"),
            ("t", "测当前节点"),
            ("T", "测整组（mihomo）"),
            ("s", "切换排序"),
            ("/", "过滤节点"),
            ("Esc", "清除过滤"),
            ("l / j / → / ↓", "组选择行进入节点"),
            ("j / k / ↓ / ↑", "上下移动（按行）"),
            ("h / l / ← / →", "同行左右移动"),
            ("h / ←", "最左列回组选择行"),
            ("gg / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Proxies => &[
            ("Enter", "分组面板：进入节点区"),
            ("Enter", "节点：选择节点"),
            ("t", "测当前节点"),
            ("T", "测整组（mihomo）"),
            ("s", "切换排序"),
            ("/", "过滤节点"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动（按行）"),
            ("h / l / ← / →", "同行左右移动"),
            ("h / ←", "最左列回分组"),
            ("l / →", "分组面板进入节点"),
            ("gg / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Providers => &[
            ("u", "更新当前 provider"),
            ("U", "更新全部 / 匹配的"),
            ("h", "健康检查代理集合"),
            ("/", "按名称过滤"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动"),
            ("gg / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Rules => &[
            ("/", "过滤规则"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动"),
            ("gg / G", "顶部 / 底部"),
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
            ("gg / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
            ("p", "暂停 / 恢复刷新"),
        ],
        Page::Logs => LOGS_KEYS,
        Page::Settings => &[
            ("Enter / Space", "切换开关或执行操作"),
            ("Enter", "端口行：编辑端口"),
            ("Enter / Esc", "编辑中提交 / 取消"),
            ("y", "确认标 ! 的操作"),
            ("n / Esc", "取消"),
            ("j / k / ↓ / ↑", "上下移动"),
            ("gg / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Subscriptions => &[
            ("Enter", "切换到这个订阅"),
            ("a", "添加：订阅地址或文件路径"),
            ("u", "更新当前订阅"),
            ("U", "更新全部 / 匹配的"),
            ("t", "开关自动更新（URL 订阅）"),
            ("i", "自动更新间隔（小时）"),
            ("e", "改名"),
            ("K", "更换 age 私钥（加密订阅）"),
            ("d", "删除（正在使用的不能删）"),
            ("/", "按名称过滤"),
            ("Esc", "清除过滤"),
            ("j / k / ↓ / ↑", "上下移动"),
            ("gg / G", "顶部 / 底部"),
            ("Ctrl+d / Ctrl+u", "翻半页"),
        ],
        Page::Overview => &[],
    }
}

/// The logs page's keys.
const LOGS_KEYS: &[(&str, &str)] = &[
    ("Enter", "查看整条日志"),
    ("y", "复制当前日志（OSC 52）"),
    ("l", "切换级别（循环）"),
    ("p", "暂停 / 恢复"),
    ("c", "清空日志"),
    ("/", "过滤日志"),
    ("Esc", "清除过滤"),
    ("j / k / ↓ / ↑", "上下移动（按条）"),
    ("gg", "最早"),
    ("G", "最新并跟随"),
    ("Ctrl+d / Ctrl+u", "翻半页"),
];

/// Keys that work on every page, as `(key, what it does)`.
const GLOBAL_KEYS: [(&str, &str); 9] = [
    ("1-8", "跳到第 N 页"),
    ("Tab / Shift+Tab", "下一页 / 上一页"),
    ("r", "刷新"),
    ("m", "切换代理模式"),
    ("P", "切换配置"),
    ("C", "切换主题"),
    ("鼠标", "点击选择 双击确认"),
    ("?", "打开 / 关闭帮助"),
    ("q / Ctrl+C", "退出"),
];

/// Widest the help gets, borders included.
const WIDTH: u16 = 74;

/// Columns of the key column: the longest keys (`Ctrl+d / Ctrl+u`) and a
/// space.
const KEY_WIDTH: usize = 16;

/// Where the help goes on a screen and how much of it shows.
struct Sheet {
    rect: Rect,
    /// Global keys on the left, the page's on the right; else one column,
    /// global keys above.
    columns: bool,
    /// Rows of content, as many as the longer column.
    rows: usize,
    /// Rows of content the panel shows.
    visible: usize,
}

/// Lays the help out in `screen` for `page`: two columns from
/// [`COMPACT_WIDTH`], one column below it.
fn sheet(screen: Rect, page: Page) -> Sheet {
    let keys = page_keys(page, proxies::stacked(screen.width)).len().max(1);
    // A heading and an empty line above each list.
    let global = 2 + GLOBAL_KEYS.len();
    let current = 2 + keys;
    let columns = screen.width >= COMPACT_WIDTH;
    let rows = if columns {
        global.max(current)
    } else {
        global + 1 + current
    };
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX);
    let rect = popup::centered(screen, WIDTH, height);
    Sheet {
        rect,
        columns,
        rows,
        visible: usize::from(rect.height.saturating_sub(2)),
    }
}

/// How far the help scrolls on `screen`: 0 when it shows everything.
pub fn max_scroll(screen: Rect, page: Page) -> usize {
    let sheet = sheet(screen, page);
    sheet.rows.saturating_sub(sheet.visible)
}

/// Rows the help shows on `screen`, for paging it.
pub fn visible_rows(screen: Rect, page: Page) -> usize {
    sheet(screen, page).visible
}

/// Centered help overlay: global keys on the left, the current page's on
/// the right; on a narrow screen one above the other. Scrolled to
/// [`App::help_scroll`] when it does not fit, with the rows in view and the
/// keys that scroll in the title: it then takes the whole height, the key
/// bar included.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let sheet = sheet(area, app.page);
    let top = app
        .help_scroll
        .min(sheet.rows.saturating_sub(sheet.visible));
    let status = (sheet.rows > sheet.visible).then(|| {
        let mut position = format!("{}–{} / {}", top + 1, top + sheet.visible, sheet.rows);
        if top > 0 {
            position.insert(0, '▴');
        }
        if top + sheet.visible < sheet.rows {
            position.push('▾');
        }
        position.push_str(" · j/k 滚动 ");
        Span::styled(position, Style::new().fg(theme.muted))
    });
    app.hits.add(sheet.rect, Target::Popup);
    let inner = popup::panel(frame, sheet.rect, theme.accent, Some("帮助"), status);
    scrollbar::render(
        frame,
        &app.hits,
        scrollbar::in_popup(inner),
        top,
        sheet.visible,
        sheet.rows,
        theme.accent,
    );
    version(frame, sheet.rect, theme.muted);

    let heading =
        |text: String| Line::styled(text, Style::new().fg(theme.fg).add_modifier(Modifier::BOLD));
    let key_line = |key: &str, description: &'static str| {
        // Padded by columns, not chars: `鼠标` is two chars but four columns.
        let pad = KEY_WIDTH.saturating_sub(key.width());
        Line::from(vec![
            Span::styled(
                format!("{key}{:pad$}", ""),
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(description, Style::new().fg(theme.muted)),
        ])
    };

    let mut global = vec![heading("全局".to_owned()), Line::raw("")];
    global.extend(
        GLOBAL_KEYS
            .into_iter()
            .map(|(key, description)| key_line(key, description)),
    );

    let keys = page_keys(app.page, proxies::stacked(area.width));
    let mut current = vec![
        heading(format!("当前页面 · {}", page_title(app.page))),
        Line::raw(""),
    ];
    if keys.is_empty() {
        current.push(Line::styled("暂无专属按键", Style::new().fg(theme.muted)));
    }
    current.extend(
        keys.iter()
            .map(|(key, description)| key_line(key, description)),
    );

    let scroll = (u16::try_from(top).unwrap_or(u16::MAX), 0);
    if sheet.columns {
        // The global keys are short: the page's get the rest, so their
        // longest description still fits at 70 columns.
        let left_width = global.iter().map(Line::width).max().unwrap_or(0) + 1;
        let [left, right] = Layout::horizontal([
            Constraint::Length(u16::try_from(left_width).unwrap_or(u16::MAX)),
            Constraint::Fill(1),
        ])
        .areas(inner);
        frame.render_widget(Paragraph::new(global).scroll(scroll), left);
        frame.render_widget(Paragraph::new(current).scroll(scroll), right);
    } else {
        global.push(Line::raw(""));
        global.extend(current);
        frame.render_widget(Paragraph::new(global).scroll(scroll), inner);
    }
}

/// `tabby <version> (<commit> <date>)` on the bottom border of `panel`, at
/// the right; just the version when the commit does not fit, nothing when
/// that does not either. The border keeps a cell and its corner on each side.
fn version(frame: &mut Frame, panel: Rect, color: Color) {
    // Versions, hashes and dates are ASCII: a byte is a column.
    let fits = |text: &String| text.len() + 4 <= usize::from(panel.width);
    let text = [env!("TABBY_LONG_VERSION"), env!("CARGO_PKG_VERSION")]
        .into_iter()
        .map(|version| format!(" tabby {version} "))
        .find(fits);
    if let Some(text) = text
        && panel.height >= 2
        && let Ok(width) = u16::try_from(text.len())
    {
        let area = Rect::new(panel.right() - 2 - width, panel.bottom() - 1, width, 1);
        frame.render_widget(Span::styled(text, Style::new().fg(color)), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_column_scrolls_as_far_as_its_last_row() {
        // Connections: 11 keys; one column holds 11 + 1 + 13 = 25 rows.
        let rows = |width, height| {
            let screen = Rect::new(0, 0, width, height);
            (
                max_scroll(screen, Page::Connections),
                visible_rows(screen, Page::Connections),
            )
        };
        // 27 rows hold the panel with its borders.
        assert_eq!(rows(69, 27), (0, 25));
        assert_eq!(rows(69, 26), (1, 24));
        assert_eq!(rows(69, 25), (2, 23));
        assert_eq!(rows(50, 15), (12, 13));
        // Two columns from 70: 13 rows and borders.
        assert_eq!(rows(70, 15), (0, 13));
        assert_eq!(rows(70, 14), (1, 12));
    }

    #[test]
    fn a_page_without_keys_still_has_its_line() {
        let screen = Rect::new(0, 0, 60, 40);
        // 11 + 1 + 3: the heading, an empty line and `暂无专属按键`.
        assert_eq!(visible_rows(screen, Page::Overview), 15);
    }
}
