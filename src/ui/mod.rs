pub mod components;
pub mod connections;
pub mod icons;
pub mod logs;
pub mod overview;
pub mod providers;
pub mod proxies;
pub mod rules;
pub mod scroll;
pub mod settings;
pub mod shadow;
pub mod text;
pub mod theme;

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Paragraph, Wrap},
};

use crate::{
    api::error::ApiError,
    app::{App, Overlay, Page},
    ui::theme::Theme,
};

/// Below this many columns the proxies page folds its group list into a
/// picker above the grid and the tabs show only their numbers (plan §7.3).
pub const NARROW_WIDTH: u16 = 100;

/// Below this many columns the connections table drops two columns, the
/// rules table's number column shrinks and the help is one column (plan
/// §7.3).
pub const COMPACT_WIDTH: u16 = 70;

/// The smallest terminal tabby draws into; below it only says so.
pub const MIN_WIDTH: u16 = 50;
pub const MIN_HEIGHT: u16 = 15;

/// Whether a terminal of `screen` is below [`MIN_WIDTH`] × [`MIN_HEIGHT`].
pub fn too_small(screen: Rect) -> bool {
    screen.width < MIN_WIDTH || screen.height < MIN_HEIGHT
}

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
    if too_small(frame.area()) {
        draw_too_small(frame, app);
    } else {
        draw_screen(frame, app);
    }
    fill_background(frame.buffer_mut(), &app.theme);
}

/// Gives every cell left on the terminal's own colors the theme's
/// background, and its body text color: the terminal's own text color need
/// not be readable on a background the theme brings. After everything
/// else, so that what popups wipe gets them too.
fn fill_background(buffer: &mut Buffer, theme: &Theme) {
    if theme.bg == Color::Reset {
        return;
    }
    for cell in &mut buffer.content {
        if cell.bg == Color::Reset {
            cell.bg = theme.bg;
        }
        if cell.fg == Color::Reset {
            cell.fg = theme.fg;
        }
    }
}

fn draw_screen(frame: &mut Frame, app: &App) {
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
        Some(Overlay::Profiles) => components::profiles::render(frame, frame.area(), app),
        Some(Overlay::Confirm) => components::confirm::render(frame, frame.area(), app),
        Some(Overlay::Groups) => proxies::render_picker(frame, regions.body, app),
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

/// The whole screen when it is too small to draw into: what it is and what
/// it needs to be, in the middle.
fn draw_too_small(frame: &mut Frame, app: &App) {
    let screen = frame.area();
    let lines = vec![
        Line::styled(
            "终端尺寸过小",
            Style::new()
                .fg(app.theme.warning)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(
                "当前 {}×{}，至少需要 {MIN_WIDTH}×{MIN_HEIGHT}",
                screen.width, screen.height
            ),
            Style::new().fg(app.theme.muted),
        ),
    ];
    // Lines wider than the screen wrap; a rough count is enough to center.
    let width = usize::from(screen.width.max(1));
    let rows: usize = lines.iter().map(|line| line.width().div_ceil(width)).sum();
    let height = u16::try_from(rows).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(lines).centered().wrap(Wrap { trim: true });
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(screen);
    frame.render_widget(paragraph, middle);
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
