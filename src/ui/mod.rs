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

use crate::{
    api::{error::ApiError, models::GLOBAL},
    app::{App, Overlay, Page},
};

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
    match app.page {
        Page::Overview => {
            if ready(frame, area, app, app.configs.as_ref(), "正在连接控制器…").is_some() {
                draw_unbuilt(frame, area, app, None);
            }
        }
        Page::Proxies => {
            if let Some(proxies) = ready(frame, area, app, app.proxies.as_ref(), "正在加载代理…")
            {
                // GLOBAL only matters in global mode.
                let global_mode =
                    matches!(&app.configs, Some(Ok(configs)) if configs.mode == "global");
                let groups = proxies
                    .groups()
                    .filter(|group| global_mode || group.name != GLOBAL)
                    .count();
                if groups == 0 {
                    components::states::empty(
                        frame,
                        area,
                        app,
                        "配置里没有代理组",
                        &[("r", "刷新")],
                    );
                } else {
                    draw_unbuilt(frame, area, app, Some(format!("{groups} 个代理组")));
                }
            }
        }
        _ => draw_unbuilt(frame, area, app, None),
    }
}

/// Returns the page's data once loaded; until then draws the loading or error
/// state into `area` and returns `None`.
fn ready<'a, T>(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    data: Option<&'a Result<T, ApiError>>,
    loading: &str,
) -> Option<&'a T> {
    match data {
        None => {
            components::states::loading(frame, area, app, loading);
            None
        }
        Some(Err(err)) => {
            components::states::error(frame, area, app, &text::describe(err), text::detail(err));
            None
        }
        Some(Ok(data)) => Some(data),
    }
}

fn draw_unbuilt(frame: &mut Frame, area: Rect, app: &App, summary: Option<String>) {
    let theme = app.theme;
    let mut lines = vec![Line::styled(
        page_title(app.page),
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
    )];
    lines.extend(summary.map(|summary| Line::styled(summary, Style::new().fg(theme.fg))));
    lines.push(Line::styled("此页面尚未实现", Style::new().fg(theme.muted)));
    let rows = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(rows),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), middle);
}
