use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

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
        Page::Proxies => vec![
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
            hint("j/k", "移动"),
            hint("h/l", "切换面板"),
        ],
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
