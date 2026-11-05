use jiff::{Timestamp, Zoned};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::error::ApiError,
    api::models::{Health, ProxyProvider, RuleProvider, Subscription},
    app::{App, ProviderKind, ProviderRef, ProviderRow, Work},
    format,
    ui::{
        components::states,
        text::{describe, detail},
        theme::Theme,
    },
};

/// Cells of the health bar.
const BAR_WIDTH: usize = 10;

/// Columns between the cells of a row.
const GAP: usize = 2;

/// The spinner and the space after it, in front of every name.
const MARK_WIDTH: usize = 2;

/// A name is never squeezed below this (or its own width, if shorter);
/// columns on the right go first.
const NAME_MIN: usize = 8;

/// A name wider than this is cut, so the columns after it keep their room.
const NAME_MAX: usize = 24;

/// Narrower than this, the subscription is left out.
const USAGE_MIN: usize = 12;

const NO_VALUE: &str = "—";

/// One list of the page, as it stands.
type Slot<'a, T> = Option<&'a Result<Vec<T>, ApiError>>;

fn panel() -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded)
}

/// Lines that fit in a page body `body`: the panel's inside.
pub fn capacity(body: Rect) -> usize {
    usize::from(panel().inner(body).height)
}

/// What one line of the panel holds. There is one for every line, and they
/// are cheap; only the lines in view are laid out into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    ProxyHeading,
    RuleHeading,
    /// Row `index` of [`crate::app::ProvidersView::rows`].
    Row(usize),
    /// What a section shows instead of rows.
    Note(Note),
    Gap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Note {
    ProxiesLoading,
    ProxiesFailed,
    NoProxies,
    NoMatchingProxies,
    RulesLoading,
    RulesFailed,
    NoRules,
    NoMatchingRules,
}

fn is_empty<T>(slot: Slot<'_, T>) -> bool {
    matches!(slot, Some(Ok(list)) if list.is_empty())
}

/// The providers page: one panel, the proxy providers above the rule
/// providers. The page is one spinner until either list is in, and the
/// error page when the proxy list failed with no rule list to show instead
/// (both failed, or original Clash, which has only the one).
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let view = &app.providers_view;
    if view.loading() {
        states::loading(frame, area, app, "正在加载 provider…");
        return;
    }
    if let Some(Err(err)) = &view.proxies
        && !matches!(view.rules, Some(Ok(_)))
    {
        states::error(frame, area, app, &describe(err), detail(err));
        return;
    }

    let theme = app.theme;
    let filter = view.filter.value();
    let mut title = vec![Span::styled(
        " Providers ",
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
    )];
    if !filter.is_empty() {
        title.push(Span::styled(
            format!(" · 过滤「{filter}」 "),
            Style::new().fg(theme.muted),
        ));
    }
    let block = panel()
        .border_style(Style::new().fg(theme.accent))
        .title(Line::from(title));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let supported = app.rule_providers_supported();
    let rows = view.rows();
    if rows.is_empty() && !filter.is_empty() {
        states::empty(
            frame,
            inner,
            app,
            &format!("没有匹配「{filter}」的 provider"),
            &[("Esc", "清除过滤"), ("/", "修改过滤")],
        );
        return;
    }
    if is_empty(view.proxies.as_ref()) && (is_empty(view.rules.as_ref()) || !supported) {
        let reason = if supported {
            "没有 provider：配置里没有 proxy-providers 和 rule-providers"
        } else {
            "没有 provider：配置里没有 proxy-providers"
        };
        states::empty(frame, inner, app, reason, &[("r", "刷新")]);
        return;
    }
    draw_lines(frame, inner, app, &rows);
}

/// The headings, rows and notes of both sections into `inner`, scrolled to
/// keep the cursor in view.
fn draw_lines(frame: &mut Frame, inner: Rect, app: &App, rows: &[ProviderRow<'_>]) {
    let theme = app.theme;
    let view = &app.providers_view;
    let filter = view.filter.value();
    let supported = app.rule_providers_supported();
    let items = items(app, rows);
    let cursor = view.cursor.min(rows.len().saturating_sub(1));
    let cursor_item = items
        .iter()
        .position(|item| *item == Item::Row(cursor))
        .unwrap_or(0);
    let window = view
        .scroll
        .window(cursor_item, items.len(), usize::from(inner.height));

    let now = Zoned::now();
    let width = usize::from(inner.width);
    let proxies = view.visible_proxies();
    let rule_list = view.visible_rules();
    let proxy_layout = proxy_layout(app, &proxies, width, &now);
    let rule_layout = rule_layout(app, &rule_list, width, &now);
    for (line_index, item) in items[window].iter().enumerate() {
        let line = match *item {
            Item::ProxyHeading => heading(
                &theme,
                "代理集合",
                proxies.len(),
                view.proxies.as_ref(),
                filter,
            ),
            Item::RuleHeading if !supported => Line::from(vec![
                Span::styled(
                    " 规则集合",
                    Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · 需要 mihomo", Style::new().fg(theme.muted)),
            ]),
            Item::RuleHeading => heading(
                &theme,
                "规则集合",
                rule_list.len(),
                view.rules.as_ref(),
                filter,
            ),
            Item::Row(index) => {
                let line = match rows[index] {
                    ProviderRow::Proxy(provider) => proxy_line(app, provider, &proxy_layout, &now),
                    ProviderRow::Rule(provider) => rule_line(app, provider, &rule_layout, &now),
                };
                if index == cursor {
                    line.style(theme.surface)
                } else {
                    line
                }
            }
            Item::Note(note) => note_line(app, note),
            Item::Gap => Line::raw(""),
        };
        let y = inner.y + u16::try_from(line_index).unwrap_or(u16::MAX);
        frame.render_widget(
            line,
            Rect {
                y,
                height: 1,
                ..inner
            },
        );
    }
}

/// Every line of the panel, top to bottom.
fn items(app: &App, rows: &[ProviderRow<'_>]) -> Vec<Item> {
    let view = &app.providers_view;
    let filtered = !view.filter.value().is_empty();
    let proxy_rows = rows
        .iter()
        .take_while(|row| matches!(row, ProviderRow::Proxy(_)))
        .count();
    let mut items = vec![Item::ProxyHeading];
    match &view.proxies {
        None => items.push(Item::Note(Note::ProxiesLoading)),
        Some(Err(_)) => items.push(Item::Note(Note::ProxiesFailed)),
        Some(Ok(list)) if list.is_empty() => items.push(Item::Note(Note::NoProxies)),
        Some(Ok(_)) if proxy_rows == 0 && filtered => {
            items.push(Item::Note(Note::NoMatchingProxies));
        }
        Some(Ok(_)) => items.extend((0..proxy_rows).map(Item::Row)),
    }
    items.extend([Item::Gap, Item::RuleHeading]);
    if !app.rule_providers_supported() {
        return items;
    }
    match &view.rules {
        None => items.push(Item::Note(Note::RulesLoading)),
        Some(Err(_)) => items.push(Item::Note(Note::RulesFailed)),
        Some(Ok(list)) if list.is_empty() => items.push(Item::Note(Note::NoRules)),
        Some(Ok(_)) if rows.len() == proxy_rows && filtered => {
            items.push(Item::Note(Note::NoMatchingRules));
        }
        Some(Ok(_)) => items.extend((proxy_rows..rows.len()).map(Item::Row)),
    }
    items
}

/// `代理集合  N`, or `m/n` under a filter.
fn heading<T>(
    theme: &Theme,
    title: &'static str,
    shown: usize,
    slot: Slot<'_, T>,
    filter: &str,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {title}"),
        Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
    )];
    if let Some(Ok(list)) = slot {
        let count = if filter.is_empty() {
            format!("  {}", list.len())
        } else {
            format!("  {shown}/{}", list.len())
        };
        spans.push(Span::styled(count, Style::new().fg(theme.muted)));
    }
    Line::from(spans)
}

fn note_line(app: &App, note: Note) -> Line<'static> {
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    let indent = || Span::raw(" ".repeat(MARK_WIDTH));
    let failed = |reason: String| {
        Line::from(vec![
            indent(),
            Span::styled(format!("✗ {reason}"), Style::new().fg(theme.error)),
            Span::styled("  按 r 重试", muted),
        ])
    };
    let text = |text: &'static str| Line::from(vec![indent(), Span::styled(text, muted)]);
    let view = &app.providers_view;
    match note {
        Note::ProxiesLoading | Note::RulesLoading => Line::from(vec![
            Span::styled(
                format!("{} ", states::spinner_frame(app)),
                Style::new().fg(theme.accent),
            ),
            Span::styled("正在加载…", muted),
        ]),
        Note::ProxiesFailed => failed(error_of(view.proxies.as_ref())),
        Note::RulesFailed => failed(error_of(view.rules.as_ref())),
        Note::NoProxies => text("配置里没有 proxy-providers"),
        Note::NoRules => text("配置里没有 rule-providers"),
        Note::NoMatchingProxies => text("没有匹配的代理集合"),
        Note::NoMatchingRules => text("没有匹配的规则集合"),
    }
}

fn error_of<T>(slot: Slot<'_, T>) -> String {
    match slot {
        Some(Err(err)) => describe(err),
        _ => String::new(),
    }
}

/// Column widths of one section, shared by its rows so they line up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    name: usize,
    /// Widths of the columns after the name, left to right; only the first
    /// `shown` of them fit.
    columns: [usize; 4],
    shown: usize,
    /// Room for the subscription, after everything else; 0 leaves it out.
    usage: usize,
}

/// Fits a row `width` columns wide: a name as wide as `name` (up to
/// [`NAME_MAX`]), then `columns`, dropped from the right while the name
/// would get fewer than [`NAME_MIN`] columns, then, if everything else fits
/// and at least [`USAGE_MIN`] columns are left, the subscription.
fn fit(width: usize, name: usize, columns: [usize; 4], usage: bool) -> Layout {
    let name = name.min(NAME_MAX);
    let cost = |shown: usize| -> usize {
        MARK_WIDTH
            + columns[..shown]
                .iter()
                .map(|width| GAP + width)
                .sum::<usize>()
    };
    let mut shown = columns.len();
    while shown > 0 && cost(shown) + name.min(NAME_MIN) > width {
        shown -= 1;
    }
    let name = name.min(width.saturating_sub(cost(shown)));
    let rest = width.saturating_sub(cost(shown) + name + GAP);
    let usage = if usage && shown == columns.len() && rest >= USAGE_MIN {
        rest
    } else {
        0
    };
    Layout {
        name,
        columns,
        shown,
        usage,
    }
}

/// The time column: what is being done to the provider, or how long ago
/// it was last loaded.
fn time_text(app: &App, target: &ProviderRef, updated: Option<Timestamp>, now: &Zoned) -> String {
    match app.providers_view.busy.get(target) {
        Some(Work::Updating) => "更新中…".to_owned(),
        Some(Work::Checking) => "检查中…".to_owned(),
        None => updated.map_or_else(|| NO_VALUE.to_owned(), |time| format::ago(time, now)),
    }
}

fn target(kind: ProviderKind, name: &str) -> ProviderRef {
    ProviderRef {
        kind,
        name: name.to_owned(),
    }
}

fn widest<'a>(texts: impl Iterator<Item = &'a str>) -> usize {
    texts.map(UnicodeWidthStr::width).max().unwrap_or(0)
}

fn widest_of(texts: &[String]) -> usize {
    widest(texts.iter().map(String::as_str))
}

fn node_count(provider: &ProxyProvider) -> String {
    format!("{} 个节点", provider.proxies.len())
}

fn rule_count(provider: &RuleProvider) -> String {
    format!("{} 条规则", provider.rule_count)
}

/// The label after the health bar: `可用 m/n`, or `未测`.
fn availability(health: Health) -> String {
    if health.tested {
        format!("可用 {}/{}", health.alive, health.total)
    } else {
        "未测".to_owned()
    }
}

fn proxy_layout(app: &App, providers: &[&ProxyProvider], width: usize, now: &Zoned) -> Layout {
    let counts: Vec<String> = providers.iter().map(|p| node_count(p)).collect();
    let labels: Vec<String> = providers.iter().map(|p| availability(p.health())).collect();
    let times: Vec<String> = providers
        .iter()
        .map(|p| {
            time_text(
                app,
                &target(ProviderKind::Proxy, &p.name),
                p.updated_at,
                now,
            )
        })
        .collect();
    fit(
        width,
        widest(providers.iter().map(|p| p.name.as_str())),
        [
            widest(providers.iter().map(|p| p.vehicle_type.as_str())),
            widest_of(&counts),
            BAR_WIDTH + 1 + widest_of(&labels),
            widest_of(&times),
        ],
        providers.iter().any(|p| p.subscription().is_some()),
    )
}

fn rule_layout(app: &App, providers: &[&RuleProvider], width: usize, now: &Zoned) -> Layout {
    let counts: Vec<String> = providers.iter().map(|p| rule_count(p)).collect();
    let times: Vec<String> = providers
        .iter()
        .map(|p| time_text(app, &target(ProviderKind::Rule, &p.name), p.updated_at, now))
        .collect();
    fit(
        width,
        widest(providers.iter().map(|p| p.name.as_str())),
        [
            widest(providers.iter().map(|p| p.vehicle_type.as_str())),
            widest(providers.iter().map(|p| p.behavior.as_str())),
            widest_of(&counts),
            widest_of(&times),
        ],
        false,
    )
}

/// `text` cut to `width` and padded to it, on the left when `right`.
fn cell(text: &str, width: usize, right: bool, style: Style) -> Span<'static> {
    let text = format::truncate(text, width);
    let pad = " ".repeat(width.saturating_sub(text.width()));
    let content = if right {
        format!("{pad}{text}")
    } else {
        format!("{text}{pad}")
    };
    Span::styled(content, style)
}

/// Where a provider loads from, colored apart.
fn tag(vehicle: &str, width: usize, theme: &Theme) -> Span<'static> {
    let color = match vehicle {
        "HTTP" => theme.c2,
        "File" => theme.c1,
        _ => theme.c4,
    };
    cell(vehicle, width, false, Style::new().fg(color))
}

/// A row: the spinner while the provider is worked on, the name, then the
/// columns that fit, `GAP` apart.
fn row(
    app: &App,
    target: &ProviderRef,
    layout: &Layout,
    columns: [Vec<Span<'static>>; 4],
) -> Vec<Span<'static>> {
    let theme = app.theme;
    let mark = if app.providers_view.busy.contains_key(target) {
        Span::styled(
            format!("{} ", states::spinner_frame(app)),
            Style::new().fg(theme.accent),
        )
    } else {
        Span::raw(" ".repeat(MARK_WIDTH))
    };
    let mut spans = vec![
        mark,
        cell(&target.name, layout.name, false, Style::new().fg(theme.fg)),
    ];
    for column in columns.into_iter().take(layout.shown) {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.extend(column);
    }
    spans
}

fn proxy_line(app: &App, provider: &ProxyProvider, layout: &Layout, now: &Zoned) -> Line<'static> {
    let theme = app.theme;
    let target = target(ProviderKind::Proxy, &provider.name);
    let [tag_width, count, health, time_width] = layout.columns;
    let time = time_text(app, &target, provider.updated_at, now);
    let mut spans = row(
        app,
        &target,
        layout,
        [
            vec![tag(&provider.vehicle_type, tag_width, &theme)],
            vec![cell(
                &node_count(provider),
                count,
                true,
                Style::new().fg(theme.fg),
            )],
            health_cells(&theme, provider.health(), health),
            vec![cell(&time, time_width, false, Style::new().fg(theme.muted))],
        ],
    );
    if layout.usage > 0
        && let Some(subscription) = provider.subscription()
    {
        spans.push(Span::raw(" ".repeat(GAP)));
        spans.push(Span::styled(
            format::truncate(&usage(subscription, now), layout.usage),
            Style::new().fg(theme.fg),
        ));
    }
    Line::from(spans)
}

fn rule_line(app: &App, provider: &RuleProvider, layout: &Layout, now: &Zoned) -> Line<'static> {
    let theme = app.theme;
    let target = target(ProviderKind::Rule, &provider.name);
    let [tag_width, behavior, count, time] = layout.columns;
    let text = time_text(app, &target, provider.updated_at, now);
    Line::from(row(
        app,
        &target,
        layout,
        [
            vec![tag(&provider.vehicle_type, tag_width, &theme)],
            vec![cell(
                &provider.behavior,
                behavior,
                false,
                Style::new().fg(theme.fg),
            )],
            vec![cell(
                &rule_count(provider),
                count,
                true,
                Style::new().fg(theme.fg),
            )],
            vec![cell(&text, time, false, Style::new().fg(theme.muted))],
        ],
    ))
}

/// How many of [`BAR_WIDTH`] cells the bar fills: the share of nodes alive,
/// rounded, but never empty while one is alive nor full while one is not.
fn filled(health: Health) -> usize {
    if health.total == 0 || health.alive == 0 {
        return 0;
    }
    let cells = (health.alive * BAR_WIDTH + health.total / 2) / health.total;
    let most = if health.alive < health.total {
        BAR_WIDTH - 1
    } else {
        BAR_WIDTH
    };
    cells.clamp(1, most)
}

/// The color of a health share: 80 % and up healthy, 50 % and up degraded,
/// below that failing.
fn health_color(health: Health, theme: &Theme) -> Color {
    if health.alive * 100 >= health.total * 80 {
        theme.success
    } else if health.alive * 100 >= health.total * 50 {
        theme.warning
    } else {
        theme.error
    }
}

/// The bar and the label after it, `width` columns in all. With no node
/// tested yet the bar is muted and says `未测`.
fn health_cells(theme: &Theme, health: Health, width: usize) -> Vec<Span<'static>> {
    let muted = Style::new().fg(theme.muted);
    let label_width = width.saturating_sub(BAR_WIDTH + 1);
    if !health.tested {
        return vec![
            Span::styled("░".repeat(BAR_WIDTH), muted),
            Span::raw(" "),
            cell("未测", label_width, false, muted),
        ];
    }
    let color = Style::new().fg(health_color(health, theme));
    let filled = filled(health);
    let count = format!("{}/{}", health.alive, health.total);
    let pad = label_width.saturating_sub(availability(health).width());
    vec![
        Span::styled("█".repeat(filled), color),
        Span::styled("░".repeat(BAR_WIDTH - filled), muted),
        Span::raw(" "),
        Span::styled("可用 ", muted),
        Span::styled(count, color),
        Span::raw(" ".repeat(pad)),
    ]
}

/// What a subscription used and when it ends: `12.4 GB / 100.0 GB ·
/// 2027-01-01 到期`. Without a total only the use, without an expiry no
/// date; the date is in `now`'s time zone.
fn usage(subscription: Subscription, now: &Zoned) -> String {
    let size = |count: i64| {
        let (number, unit) = format::bytes(u64::try_from(count).unwrap_or(0));
        format!("{number} {unit}")
    };
    let used = size(subscription.upload.saturating_add(subscription.download));
    let text = if subscription.total > 0 {
        format!("{used} / {}", size(subscription.total))
    } else {
        format!("已用 {used}")
    };
    if subscription.expire > 0
        && let Ok(expire) = Timestamp::from_second(subscription.expire)
    {
        let date = expire.to_zoned(now.time_zone().clone()).date();
        return format!("{text} · {date} 到期");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(alive: usize, total: usize) -> Health {
        Health {
            alive,
            total,
            tested: true,
        }
    }

    #[test]
    fn the_bar_rounds_but_never_hides_a_live_or_a_dead_node() {
        assert_eq!(filled(health(2, 3)), 7);
        assert_eq!(filled(health(1, 30)), 1);
        assert_eq!(filled(health(29, 30)), 9);
        assert_eq!(filled(health(30, 30)), 10);
        assert_eq!(filled(health(0, 30)), 0);
        assert_eq!(filled(health(0, 0)), 0);
    }

    #[test]
    fn health_colors_follow_the_share_alive() {
        let theme = Theme::from(crate::config::ThemeName::Terminal);
        assert_eq!(health_color(health(4, 5), &theme), theme.success);
        assert_eq!(health_color(health(2, 3), &theme), theme.warning);
        assert_eq!(health_color(health(1, 2), &theme), theme.warning);
        assert_eq!(health_color(health(1, 3), &theme), theme.error);
    }

    #[test]
    fn rows_never_run_past_their_width() {
        // `HTTP`, `3 个节点`, the bar and `可用 2/3`, `3 小时前`.
        let columns = [4, 8, BAR_WIDTH + 1 + 7, 8];
        for width in 0..120 {
            for name in [0, 4, 9, 40] {
                let layout = fit(width, name, columns, true);
                let used = MARK_WIDTH
                    + layout.name
                    + layout.columns[..layout.shown]
                        .iter()
                        .map(|column| GAP + column)
                        .sum::<usize>()
                    + if layout.usage > 0 {
                        GAP + layout.usage
                    } else {
                        0
                    };
                assert!(used <= width.max(MARK_WIDTH), "{width} {name}");
                // A 60-column terminal leaves 58 inside the panel: room for
                // every column but the subscription.
                if width >= 58 {
                    assert_eq!(layout.shown, 4, "{width} {name}");
                }
            }
        }
    }

    #[test]
    fn usage_shows_what_the_server_reported() {
        let now: Zoned = "2026-11-05T12:00:00+08:00[Asia/Shanghai]".parse().unwrap();
        let full = Subscription {
            upload: 1_288_490_188,
            download: 12_079_595_520,
            total: 107_374_182_400,
            expire: 1_798_732_800,
        };
        assert_eq!(usage(full, &now), "12.4 GB / 100.0 GB · 2027-01-01 到期");
        let open = Subscription {
            total: 0,
            expire: 0,
            ..full
        };
        assert_eq!(usage(open, &now), "已用 12.4 GB");
    }
}
