use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Overlay, Page};

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

/// Keys while the help overlay is open.
fn help_hints() -> Vec<Hint> {
    vec![hint("Esc/?", "关闭帮助"), hint("q", "退出")]
}

/// Keys that only work on the given page.
fn page_hints(app: &App) -> Vec<Hint> {
    match app.page {
        Page::Proxies => {
            let mut hints = vec![
                hint("Enter", "选择"),
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
                hint("j/k", "移动"),
                hint("h/l", "切换面板"),
            ];
            if !app.proxies_view.filter.value().is_empty() {
                hints.push(hint("Esc", "清除过滤"));
            }
            hints
        }
        _ => vec![],
    }
}

/// Keys that work on every page.
fn global_hints() -> Vec<Hint> {
    vec![
        hint("1-7/Tab", "切换页面"),
        hint("r", "刷新"),
        hint("?", "帮助"),
        hint("q", "退出"),
    ]
}

fn hints(app: &App) -> Vec<Hint> {
    match app.overlay {
        Some(Overlay::Help) => help_hints(),
        None => {
            let mut hints = page_hints(app);
            hints.extend(global_hints());
            hints
        }
    }
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

    if app.page == Page::Proxies && app.proxies_view.typing {
        render_filter(frame, hints_area, app);
        return;
    }
    let hints = hints(app);
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for (index, hint) in hints.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        let key_style = if hint.enabled {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        spans.push(Span::styled(hint.key, key_style));
        spans.push(Span::styled(
            format!(" {}", hint.description),
            Style::new().fg(theme.muted),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), hints_area);
}

/// The filter box that replaces the hints while typing: `/ text`, with the
/// terminal cursor at the insertion point.
fn render_filter(frame: &mut Frame, area: Rect, app: &App) {
    const PROMPT: &str = "/ ";
    const HINT: &str = "  Enter 确定   Esc 清除";

    let theme = app.theme;
    let input = &app.proxies_view.filter;
    let room = usize::from(area.width).saturating_sub(PROMPT.len() + HINT.width());
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
        .collect();

    let line = Line::from(vec![
        Span::styled(
            PROMPT,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(visible, Style::new().fg(theme.fg)),
        Span::styled(HINT, Style::new().fg(theme.muted)),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    let cursor_x = area.x
        + u16::try_from(PROMPT.len() + input.visual_cursor().saturating_sub(scroll)).unwrap_or(0);
    frame.set_cursor_position((cursor_x, area.y));
}
