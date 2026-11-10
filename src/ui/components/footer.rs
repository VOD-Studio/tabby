use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tui_input::Input;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, Confirm, Gate, Overlay, Page, Pane, Setting},
    core::{DNS_KEYS, DnsKind},
    ui::proxies,
};

/// Between two hints.
const SEPARATOR: &str = "   ";
/// Stands in for the hints left out, before the ones always shown.
const MORE: &str = "…";

/// One entry of the key bar. A disabled one is shown dimmed: the key exists
/// but the connected kernel cannot do it.
struct Hint {
    key: &'static str,
    description: &'static str,
    enabled: bool,
}

const fn hint(key: &'static str, description: &'static str) -> Hint {
    Hint {
        key,
        description,
        enabled: true,
    }
}

/// Keys while the help overlay is open. When it scrolls it covers this bar
/// and says so in its title.
fn help_hints() -> Vec<Hint> {
    vec![hint("Esc/?", "关闭帮助"), hint("q", "退出")]
}

/// Keys while the connection detail popup is open.
fn detail_hints() -> Vec<Hint> {
    vec![hint("Esc", "关闭"), hint("q", "退出")]
}

/// Keys while the confirm dialog is open.
fn confirm_hints(app: &App) -> Vec<Hint> {
    let verb = app.pending.as_ref().map_or("确认", Confirm::verb);
    vec![hint("y", verb), hint("n/Esc", "取消")]
}

/// Keys while the profile popup is open; without a profile it only closes.
fn profile_hints(app: &App) -> Vec<Hint> {
    if app.profiles.is_empty() {
        return vec![hint("Esc", "关闭"), hint("q", "退出")];
    }
    vec![
        hint("Enter", "切换"),
        hint("j/k", "移动"),
        hint("Esc", "关闭"),
        hint("q", "退出"),
    ]
}

/// Keys while the theme picker is open: moving previews, `Enter` keeps.
fn theme_hints() -> Vec<Hint> {
    vec![
        hint("Enter", "使用并保存"),
        hint("j/k", "预览"),
        hint("Esc", "取消"),
        hint("q", "退出"),
    ]
}

/// Keys while the group picker of a narrow proxies page is open.
fn picker_hints() -> Vec<Hint> {
    vec![
        hint("Enter", "选择"),
        hint("j/k", "移动"),
        hint("Esc", "关闭"),
        hint("q", "退出"),
    ]
}

/// Keys while a port is being typed on the settings page.
fn port_hints() -> Vec<Hint> {
    vec![hint("Enter", "确定"), hint("Esc", "取消")]
}

/// Keys while the DNS popup is open: typing a value, or moving among the
/// keys.
fn dns_hints(app: &App) -> Vec<Hint> {
    if app.dns_view.editing.is_some() {
        return vec![
            hint("Enter", "确定"),
            hint("Esc", "取消"),
            hint("空", "跟随订阅"),
        ];
    }
    let key = DNS_KEYS[app.dns_view.cursor.min(DNS_KEYS.len() - 1)];
    let description = match DnsKind::of(key) {
        DnsKind::Switch | DnsKind::Choice(_) => "切换",
        DnsKind::Text | DnsKind::List => "编辑",
    };
    vec![
        hint("Enter", description),
        hint("j/k", "移动"),
        hint("Esc", "关闭"),
        hint("q", "退出"),
    ]
}

/// Keys that only work on the given page.
fn page_hints(app: &App) -> Vec<Hint> {
    let mut hints = match app.page {
        Page::Proxies => {
            let stacked = proxies::stacked(app.size.width);
            let groups = app.proxies_view.focus == Pane::Groups;
            let mut hints = vec![
                hint(
                    "Enter",
                    if stacked && groups {
                        "选组"
                    } else {
                        "选择"
                    },
                ),
                hint("t", "测速"),
                Hint {
                    key: "T",
                    description: if app.is_mihomo() {
                        "整组测速"
                    } else {
                        "整组测速（需要 mihomo）"
                    },
                    enabled: app.is_mihomo(),
                },
                hint("s", "排序"),
                hint("/", "过滤"),
            ];
            hints.extend(match (groups, stacked) {
                // The picker row has nothing to move through.
                (true, true) => vec![hint("l/j", "进入节点")],
                (true, false) => vec![hint("j/k", "移动"), hint("l", "进入节点")],
                (false, _) => vec![hint("h/j/k/l", "移动"), hint("h", "最左列回分组")],
            });
            hints
        }
        Page::Connections => vec![
            hint("Enter", "详情"),
            hint("x", "关闭"),
            hint(
                "X",
                if app.connections_view.filter.value().is_empty() {
                    "全部关闭"
                } else {
                    "关闭匹配"
                },
            ),
            hint("s", "排序"),
            hint("S", "反转"),
            hint("/", "过滤"),
            hint("j/k", "移动"),
            hint("gg/G", "首尾"),
            hint(
                "p",
                if app.connections_view.paused {
                    "恢复"
                } else {
                    "暂停"
                },
            ),
        ],
        Page::Subscriptions => subscription_hints(app),
        Page::Providers => vec![
            hint("u", "更新"),
            hint(
                "U",
                if app.providers_view.filter.value().is_empty() {
                    "全部更新"
                } else {
                    "更新匹配"
                },
            ),
            hint("h", "健康检查"),
            hint("/", "过滤"),
            hint("j/k", "移动"),
        ],
        Page::Rules => vec![hint("/", "过滤"), hint("j/k", "移动"), hint("gg/G", "首尾")],
        Page::Settings => settings_hints(app),
        Page::Logs => {
            let view = &app.logs_view;
            let mut hints = vec![
                hint("l", "级别"),
                hint("p", if view.paused { "恢复" } else { "暂停" }),
                hint("c", "清空"),
                hint("/", "过滤"),
            ];
            // Only while it stays put: following, it is already there.
            if view.cursor.is_some() {
                hints.push(hint("G", "最新"));
            }
            hints
        }
        Page::Overview => vec![],
    };
    if app
        .filter()
        .is_some_and(|filter| !filter.value().is_empty())
    {
        hints.push(hint("Esc", "清除过滤"));
    }
    hints
}

/// The subscriptions page's keys; an external controller has none.
fn subscription_hints(app: &App) -> Vec<Hint> {
    if !app.is_managed() {
        return vec![];
    }
    vec![
        hint("Enter", "切换"),
        hint("a", "添加"),
        hint("u", "更新"),
        hint(
            "U",
            if app.subscriptions_view.filter.value().is_empty() {
                "全部更新"
            } else {
                "更新匹配"
            },
        ),
        hint("t", "自动更新"),
        hint("i", "间隔"),
        hint("e", "改名"),
        hint("d", "删除"),
        hint("/", "过滤"),
        hint("j/k", "移动"),
    ]
}

/// The settings page's keys: what `Enter` does to the setting under the
/// cursor, dimmed where the kernel lacks it.
fn settings_hints(app: &App) -> Vec<Hint> {
    let setting = app.selected_setting();
    let description = match setting {
        Setting::Mode => "切换模式",
        Setting::LogLevel => "切换级别",
        Setting::Port(_) => "修改端口",
        Setting::AllowLan
        | Setting::Ipv6
        | Setting::Tun
        | Setting::LoginStart
        | Setting::SystemProxy
        | Setting::SubscriptionDownload => "切换",
        Setting::Reload
        | Setting::FlushFakeIp
        | Setting::UpdateGeo
        | Setting::Restart
        | Setting::Upgrade => "执行",
        Setting::StopCore => "停止",
        Setting::Dns => "编辑",
        Setting::UpgradeCore => "升级",
        Setting::CoreBinary => "—",
    };
    let enabled = match &app.configs {
        Some(Ok(configs)) => {
            app.gate(setting, configs) == Gate::Open && setting != Setting::CoreBinary
        }
        _ => false,
    };
    vec![
        Hint {
            key: "Enter",
            description,
            enabled,
        },
        hint("j/k", "移动"),
        hint("gg/G", "首尾"),
    ]
}

/// Keys that work on every page.
fn global_hints(page: Page) -> Vec<Hint> {
    let mut hints = vec![hint("1-8/Tab", "切换页面")];
    // Only where it has something to show; the proxies bar is already full.
    if page == Page::Overview {
        hints.push(hint("m", "切换模式"));
    }
    hints.extend([
        hint("r", "刷新"),
        hint("P", "配置"),
        hint("C", "主题"),
        hint("?", "帮助"),
        hint("q", "退出"),
    ]);
    hints
}

fn hints(app: &App) -> Vec<Hint> {
    match app.overlay {
        Some(Overlay::Help) => help_hints(),
        Some(Overlay::Connection) => detail_hints(),
        Some(Overlay::Confirm) => confirm_hints(app),
        Some(Overlay::Profiles) => profile_hints(app),
        Some(Overlay::Dns) => dns_hints(app),
        Some(Overlay::Themes) => theme_hints(),
        Some(Overlay::Groups) => picker_hints(),
        None if app.page == Page::Settings && app.typing() => port_hints(),
        None => {
            let mut hints = page_hints(app);
            hints.extend(global_hints(app.page));
            hints
        }
    }
}

impl Hint {
    fn width(&self) -> usize {
        self.key.width() + 1 + self.description.width()
    }
}

/// Columns `widths` take on one line, a separator between each two.
fn line_width(widths: impl IntoIterator<Item = usize>) -> usize {
    let (total, count) = widths
        .into_iter()
        .fold((0, 0_usize), |(total, count), width| {
            (total + width, count + 1)
        });
    total + SEPARATOR.len() * count.saturating_sub(1)
}

/// How many leading hints of `widths` (their widths, in order) fit in
/// `width` columns ahead of the last `pinned`, which always show, and
/// whether any was left out. The ones left out go from the end, as pages
/// list their keys most used first; [`MORE`] then stands where they were.
fn fit(widths: &[usize], pinned: usize, width: usize) -> (usize, bool) {
    let (head, tail) = widths.split_at(widths.len().saturating_sub(pinned));
    if line_width(widths.iter().copied()) <= width {
        return (head.len(), false);
    }
    let shown = (0..head.len())
        .rev()
        .find(|&kept| {
            let items = head[..kept].iter().copied().chain([MORE.width()]);
            line_width(items.chain(tail.iter().copied())) <= width
        })
        .unwrap_or(0);
    (shown, true)
}

/// Divider plus one row of key hints: keys in accent, descriptions muted.
/// Needs two rows.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let divider = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(theme.muted));
    let hints_area = divider.inner(area);
    frame.render_widget(divider, area);

    if let Some(filter) = app.filter().filter(|_| app.typing()) {
        render_filter(frame, hints_area, app, filter);
        return;
    }
    let hints = hints(app);
    // `?` and `q` stay whatever else goes: they lead to the rest.
    let pinned = hints
        .iter()
        .rev()
        .take_while(|hint| matches!(hint.key, "?" | "q"))
        .count();
    let widths: Vec<usize> = hints.iter().map(Hint::width).collect();
    let (shown, cut) = fit(&widths, pinned, usize::from(hints_area.width));
    let tail = hints.len() - pinned;
    // `None` is where the hints left out were.
    let mut items: Vec<Option<Hint>> = Vec::with_capacity(hints.len() + 1);
    for (index, hint) in hints.into_iter().enumerate() {
        if index == tail && cut {
            items.push(None);
        }
        if index < shown || index >= tail {
            items.push(Some(hint));
        }
    }
    if cut && pinned == 0 {
        items.push(None);
    }

    let muted = Style::new().fg(theme.muted);
    let mut spans = Vec::with_capacity(items.len() * 3);
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(SEPARATOR));
        }
        let Some(hint) = item else {
            spans.push(Span::styled(MORE, muted));
            continue;
        };
        let key_style = if hint.enabled {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            muted
        };
        spans.push(Span::styled(hint.key, key_style));
        spans.push(Span::styled(format!(" {}", hint.description), muted));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), hints_area);
}

/// The filter box that replaces the hints while typing: `/ text`, with the
/// terminal cursor at the insertion point.
fn render_filter(frame: &mut Frame, area: Rect, app: &App, input: &Input) {
    // The same box takes what the subscriptions page asks for.
    let asked = app.subscription_prompt();
    let secret = asked.is_some_and(|prompt| prompt.secret);
    let (prompt, hint) = match asked {
        Some(asked) => (asked.glyph, asked.hint),
        None => ("/ ", "  Enter 确定   Esc 清除"),
    };

    let theme = app.theme;
    let room = usize::from(area.width).saturating_sub(prompt.width() + hint.width());
    let scroll = input.visual_scroll(room);

    // Drop the leading columns that scrolled out of the box.
    let mut skipped = 0;
    let visible: String = input
        .value()
        .chars()
        .skip_while(|ch| {
            let skip = skipped < scroll;
            skipped += ch.width().unwrap_or(0);
            skip
        })
        // A key is not shown as it is typed.
        .map(|ch| if secret { '*' } else { ch })
        .collect();

    let text = match asked {
        Some(asked) if visible.is_empty() => {
            Span::styled(asked.placeholder, Style::new().fg(theme.muted))
        }
        _ => Span::styled(visible, Style::new().fg(theme.fg)),
    };
    let line = Line::from(vec![
        Span::styled(
            prompt,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        text,
        Span::styled(hint, Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    let cursor_x = area.x
        + u16::try_from(prompt.width() + input.visual_cursor().saturating_sub(scroll)).unwrap_or(0);
    frame.set_cursor_position((cursor_x, area.y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_go_from_the_end_and_the_pinned_ones_stay() {
        // `Enter 详情`, `s 排序`, `? 帮助`, `q 退出`, the last two pinned.
        let widths = [8, 6, 6, 6];
        let fit = |width| fit(&widths, 2, width);
        // 26 columns and three separators.
        assert_eq!(fit(35), (2, false));
        assert_eq!(fit(34), (1, true));
        // `Enter 详情   …   ? 帮助   q 退出`.
        assert_eq!(fit(30), (1, true));
        assert_eq!(fit(29), (0, true));
        // `…   ? 帮助   q 退出`, and below that still only the pinned ones.
        assert_eq!(fit(19), (0, true));
        assert_eq!(fit(10), (0, true));
    }

    #[test]
    fn widths_count_columns_not_bytes() {
        assert_eq!(hint("?", "帮助").width(), 6);
        assert_eq!(line_width([6, 6]), 15);
        assert_eq!(line_width([]), 0);
    }
}
