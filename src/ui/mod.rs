pub mod components;
pub mod connections;
pub mod logs;
pub mod overview;
pub mod providers;
pub mod proxies;
pub mod rules;
pub mod scroll;
pub mod settings;
pub mod text;
pub mod theme;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
};

use crate::{
    api::error::ApiError,
    app::{App, Overlay, Page},
};

/// The bands the screen is split into. Drawing and cursor paging both read
/// this, so the app's idea of what fits is what is drawn.
pub struct Regions {
    header: Rect,
    tabs: Rect,
    /// The reconnect banner, while there is one.
    banner: Option<Rect>,
    /// Where the page draws.
    pub body: Rect,
    footer: Rect,
}

pub fn regions(screen: Rect, reconnecting: bool) -> Regions {
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(screen);
    let (banner, body) = if reconnecting {
        let [banner, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(body);
        (Some(banner), body)
    } else {
        (None, body)
    };
    Regions {
        header,
        tabs,
        banner,
        body,
        footer,
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let regions = regions(frame.area(), app.reconnect.is_some());
    components::header::render(frame, regions.header, app);
    components::tabs::render(frame, regions.tabs, app);
    if let (Some(banner), Some(reconnect)) = (regions.banner, &app.reconnect) {
        components::banner::render(frame, banner, app, reconnect);
    }
    draw_page(frame, regions.body, app);
    components::footer::render(frame, regions.footer, app);
    match app.overlay {
        Some(Overlay::Help) => components::help::render(frame, frame.area(), app),
        Some(Overlay::Connection) => connections::render_detail(frame, frame.area(), app),
        Some(Overlay::Confirm) => components::confirm::render(frame, frame.area(), app),
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

/// The current page into the body, through its loading and error states.
fn draw_page(frame: &mut Frame, area: Rect, app: &App) {
    match app.page {
        Page::Overview => {
            if let Some(configs) = ready(frame, area, app, app.configs.as_ref(), "正在连接控制器…")
            {
                overview::render(frame, area, app, configs);
            }
        }
        Page::Proxies => {
            if let Some(proxies) = ready(frame, area, app, app.proxies.as_ref(), "正在加载代理…")
            {
                proxies::render(frame, area, app, proxies);
            }
        }
        Page::Providers => providers::render(frame, area, app),
        Page::Rules => {
            if let Some(rows) = ready(
                frame,
                area,
                app,
                app.rules_view.rows.as_ref(),
                "正在加载规则…",
            ) {
                rules::render(frame, area, app, rows);
            }
        }
        Page::Connections => connections::render(frame, area, app),
        Page::Logs => logs::render(frame, area, app),
        Page::Settings => {
            if let Some(configs) = ready(frame, area, app, app.configs.as_ref(), "正在读取设置…")
            {
                settings::render(frame, area, app, configs);
            }
        }
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
