pub mod components;
pub mod theme;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

use crate::app::{App, Page};

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, tabs, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .areas(frame.area());
    components::header::render(frame, header, app);
    components::tabs::render(frame, tabs, app);
    draw_page(frame, body, app);
}

pub fn page_title(page: Page) -> &'static str {
    match page {
        Page::Overview => "概览",
        Page::Proxies => "代理",
        Page::Providers => "Providers",
        Page::Rules => "规则",
        Page::Connections => "连接",
        Page::Logs => "日志",
        Page::Settings => "设置",
    }
}

/// Every page body is still to come; routing is real, content is not.
fn draw_page(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Fill(1),
    ])
    .areas(area);
    let lines = vec![
        Line::styled(
            page_title(app.page),
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Line::styled("此页面尚未实现", Style::new().fg(theme.muted)),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), middle);
}
