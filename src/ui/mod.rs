pub mod components;
pub mod text;
pub mod theme;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

use crate::app::{App, Overlay, Page};

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    components::header::render(frame, header, app);
    components::tabs::render(frame, tabs, app);
    let body = match &app.reconnect {
        Some(reconnect) => {
            let [banner, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(body);
            components::banner::render(frame, banner, app, reconnect);
            rest
        }
        None => body,
    };
    draw_page(frame, body, app);
    components::footer::render(frame, footer, app);
    match app.overlay {
        Some(Overlay::Help) => components::help::render(frame, frame.area(), app),
        None => {}
    }
    // Below the header row, over whatever lies underneath.
    let screen = frame.area();
    let toasts = Rect {
        y: screen.y + 1,
        height: screen.height.saturating_sub(1),
        ..screen
    };
    components::toast::render(frame, toasts, app);
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

/// Page bodies are still to come; routing and the data states are real.
fn draw_page(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    if app.page == Page::Overview {
        match &app.configs {
            None => return components::states::loading(frame, area, app, "正在连接控制器…"),
            Some(Err(err)) => {
                return components::states::error(
                    frame,
                    area,
                    app,
                    &text::describe(err),
                    text::detail(err),
                );
            }
            Some(Ok(_)) => {}
        }
    }
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
