use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType},
};
use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models::Configs,
    app::{App, Gate, Setting},
    core::{Route, shown_proxy},
    format,
    ui::{
        components::{scrollbar, states, title},
        hit::Target,
    },
};

/// The spinner and the space after it, in front of every name.
const MARK_WIDTH: usize = 2;

/// Columns kept free at the right edge, after the value.
const RIGHT_PAD: usize = 1;

/// Room the port box takes at least: five digits and the cursor.
const PORT_BOX: usize = 6;

const NO_VALUE: &str = "—";

fn panel() -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded)
}

/// Lines that fit in a page body `body`: the panel's inside.
pub fn capacity(body: Rect) -> usize {
    usize::from(panel().inner(body).height)
}

/// What one line of the panel holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Heading(&'static str),
    /// Index into the rows [`App::settings`] lists.
    Row(usize),
    Gap,
}

/// Every line of the panel: each group's heading, its settings, and a blank
/// line before the next group.
fn items(rows: &[Setting]) -> Vec<Item> {
    let mut items = Vec::with_capacity(rows.len() + 8);
    let mut section = None;
    for (index, setting) in rows.iter().enumerate() {
        let current = setting.section();
        if section != Some(current) {
            if section.is_some() {
                items.push(Item::Gap);
            }
            items.push(Item::Heading(current));
            section = Some(current);
        }
        items.push(Item::Row(index));
    }
    items
}

/// The settings page: one panel, grouped, a name on the left of each line
/// and its value against the right edge.
pub fn render(frame: &mut Frame, area: Rect, app: &App, configs: &Configs) {
    let theme = app.theme;
    let block = title::titled(
        panel().border_style(Style::new().fg(theme.accent)),
        area.width,
        vec![Span::styled(
            " 设置 ",
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        )],
        vec![],
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.hits.add(inner, Target::List);

    let view = &app.settings_view;
    let rows = app.settings();
    let items = items(&rows);
    let cursor = view.cursor.min(rows.len() - 1);
    let row_item = items
        .iter()
        .position(|item| *item == Item::Row(cursor))
        .unwrap_or(0);
    let capacity = usize::from(inner.height);
    // Moving up onto the first setting of a group brings its heading into
    // view as well; the setting itself stays in view either way.
    if let Some(heading) = row_item
        .checked_sub(1)
        .filter(|above| matches!(items[*above], Item::Heading(_)))
    {
        view.scroll.window(heading, items.len(), capacity);
    }
    let window = view.scroll.window(row_item, items.len(), capacity);
    scrollbar::list(frame, inner, &window, items.len(), theme.accent);

    let width = usize::from(inner.width);
    for (line_index, index) in window.enumerate() {
        let y = inner.y + u16::try_from(line_index).unwrap_or(u16::MAX);
        let rect = Rect {
            y,
            height: 1,
            ..inner
        };
        let line = match items[index] {
            Item::Heading(title) => Line::styled(
                format!(" {title}"),
                Style::new().fg(theme.muted).add_modifier(Modifier::BOLD),
            ),
            Item::Gap => Line::raw(""),
            Item::Row(row) => {
                let setting = rows[row];
                let editing = view
                    .editing
                    .as_ref()
                    .filter(|(field, _)| row == cursor && setting == field.setting())
                    .map(|(_, input)| input);
                let (line, caret) = setting_line(app, configs, setting, width, editing);
                if let Some(caret) = caret {
                    let x = inner.x + u16::try_from(caret).unwrap_or(u16::MAX);
                    frame.set_cursor_position((x, y));
                }
                line
            }
        };
        frame.render_widget(line, rect);
        if let Item::Row(row) = items[index] {
            app.hits.add(rect, Target::Setting(rows[row]));
        }
        if items[index] == Item::Row(cursor) {
            theme.paint_cursor(frame.buffer_mut(), rect);
        }
    }
}

/// One setting `width` columns wide, and the column of the terminal cursor
/// when its port is being typed.
fn setting_line(
    app: &App,
    configs: &Configs,
    setting: Setting,
    width: usize,
    editing: Option<&Input>,
) -> (Line<'static>, Option<usize>) {
    let theme = app.theme;
    let gate = app.gate(setting, configs);
    let busy = app.is_changing(setting);
    let mark = if busy {
        Span::styled(
            format!("{} ", states::spinner_frame(app)),
            Style::new().fg(theme.accent),
        )
    } else {
        Span::raw(" ".repeat(MARK_WIDTH))
    };
    let name_style = if gate == Gate::Open {
        Style::new().fg(theme.fg)
    } else {
        Style::new().fg(theme.muted)
    };
    let mut left = vec![mark, Span::styled(setting.name(), name_style)];
    if setting.asks() && gate == Gate::Open {
        left.push(Span::styled(
            " !",
            Style::new().fg(theme.warning).add_modifier(Modifier::BOLD),
        ));
    }
    let used: usize = left.iter().map(Span::width).sum();
    // At least one column between the name and the value.
    let room = width.saturating_sub(used + 1 + RIGHT_PAD);

    if let Some(input) = editing {
        // A port is a few digits; an address takes what room there is.
        let least = if setting == Setting::SubscriptionDownload {
            room
        } else {
            PORT_BOX
        };
        let shown = room.min(least.max(input.value().width() + 1));
        let scroll = input.visual_scroll(shown.saturating_sub(1));
        let text: String = input.value().chars().skip(scroll).collect();
        let field = format!("{text:<shown$}");
        let start = width.saturating_sub(shown + RIGHT_PAD);
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(start.saturating_sub(used))));
        spans.push(Span::styled(
            format::truncate(&field, shown),
            Style::new()
                .fg(theme.accent)
                .add_modifier(Modifier::UNDERLINED),
        ));
        let caret = start + input.visual_cursor().saturating_sub(scroll);
        return (Line::from(spans), Some(caret));
    }

    let value = fit(value(app, configs, setting, gate, busy), room);
    let value_width: usize = value.iter().map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + value_width + RIGHT_PAD)),
    ));
    spans.extend(value);
    (Line::from(spans), None)
}

/// What a setting shows on the right.
fn value(
    app: &App,
    configs: &Configs,
    setting: Setting,
    gate: Gate,
    busy: bool,
) -> Vec<Span<'static>> {
    let theme = app.theme;
    let muted = Style::new().fg(theme.muted);
    match gate {
        Gate::Open => {}
        Gate::Missing => return vec![Span::styled(NO_VALUE, muted)],
        Gate::NeedsMihomo => return vec![Span::styled("需要 mihomo", muted)],
        Gate::SelfManaged => return vec![Span::styled("由你自己管理", muted)],
        Gate::MixedOnly => return vec![Span::styled("托管内核只用 Mixed 端口", muted)],
        Gate::NeedsSystemd => return vec![Span::styled("需要 systemd", muted)],
        Gate::NoDesktopProxy => {
            return vec![Span::styled("当前桌面不支持系统代理", muted)];
        }
    }
    let fg = Style::new().fg(theme.fg);
    match setting {
        Setting::Mode => {
            // A mode tabby does not know is shown as the kernel spells it.
            let mode = configs
                .mode()
                .map_or_else(|| configs.mode.clone(), |mode| mode.name().to_uppercase());
            vec![Span::styled(mode, fg.add_modifier(Modifier::BOLD))]
        }
        Setting::AllowLan => vec![switch(app, configs.allow_lan == Some(true))],
        Setting::Ipv6 => vec![switch(app, configs.ipv6 == Some(true))],
        Setting::LogLevel => vec![Span::styled(configs.log_level.clone(), fg)],
        Setting::Port(port) => match port.of(configs) {
            Some(0) | None => vec![Span::styled("未启用", muted)],
            Some(number) => vec![Span::styled(number.to_string(), fg)],
        },
        Setting::Tun => {
            let Some(tun) = &configs.tun else {
                return vec![Span::styled(NO_VALUE, muted)];
            };
            let mut spans = vec![switch(app, tun.enable)];
            let about: Vec<&str> = [tun.stack.as_str(), tun.device.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect();
            if !about.is_empty() {
                spans.push(Span::styled(format!("  {}", about.join(" · ")), muted));
            }
            spans
        }
        Setting::SystemProxy => match app.system_proxy {
            Some(state) => vec![switch(app, state.enabled)],
            None => vec![Span::styled(NO_VALUE, muted)],
        },
        Setting::LoginStart => match app.login_start {
            Some(state) => vec![switch(app, state.enabled)],
            None => vec![Span::styled(NO_VALUE, muted)],
        },
        Setting::Dns => match app.dns_overrides().len() {
            0 => vec![Span::styled("跟随订阅", muted)],
            count => vec![Span::styled(format!("{count} 项已覆盖"), fg)],
        },
        Setting::SubscriptionDownload => match app.subscription_download() {
            None => vec![Span::styled(NO_VALUE, muted)],
            Some((download, port)) => match download.via {
                Route::Direct => vec![Span::styled("直连", fg)],
                Route::Core => vec![
                    Span::styled("经托管内核", fg),
                    Span::styled(format!("  127.0.0.1:{port}"), muted),
                ],
                Route::Proxy => vec![
                    Span::styled("经代理", fg),
                    Span::styled(format!("  {}", shown_proxy(&download.proxy)), muted),
                ],
            },
        },
        Setting::CoreBinary => {
            let Some(path) = app.core_binary() else {
                return vec![Span::styled(NO_VALUE, muted)];
            };
            // The version first: a path too long for the row is cut, and
            // the version is what is looked for.
            let mut spans = Vec::new();
            if let Some(Ok(version)) = &app.version {
                spans.push(Span::styled(format!("{}  ", version.version), muted));
            }
            spans.push(Span::styled(path.display().to_string(), fg));
            spans
        }
        Setting::Reload
        | Setting::FlushFakeIp
        | Setting::UpdateGeo
        | Setting::Restart
        | Setting::Upgrade
        | Setting::UpgradeCore
        | Setting::StopCore => {
            if busy {
                vec![Span::styled("进行中…", muted)]
            } else {
                vec![]
            }
        }
    }
}

/// `● 开` in success, `○ 关` in muted.
fn switch(app: &App, on: bool) -> Span<'static> {
    if on {
        Span::styled(
            format!("{} 开", app.icons.on),
            Style::new().fg(app.theme.success),
        )
    } else {
        Span::styled(
            format!("{} 关", app.icons.off),
            Style::new().fg(app.theme.muted),
        )
    }
}

/// `spans` cut to `width` columns: the first ones are kept whole while they
/// fit, the one that does not is cut with `…`, the rest are dropped.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut left = width;
    let mut kept = Vec::with_capacity(spans.len());
    for span in spans {
        let span_width = span.width();
        if span_width <= left {
            left -= span_width;
            kept.push(span);
        } else {
            if left > 0 {
                kept.push(Span::styled(
                    format::truncate(&span.content, left),
                    span.style,
                ));
            }
            break;
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_is_listed_once_under_its_heading() {
        let headings = |rows: &[Setting]| -> (Vec<usize>, Vec<&'static str>) {
            let items = items(rows);
            let indices = items
                .iter()
                .filter_map(|item| match item {
                    Item::Row(index) => Some(*index),
                    _ => None,
                })
                .collect();
            let headings = items
                .iter()
                .filter_map(|item| match item {
                    Item::Heading(title) => Some(*title),
                    _ => None,
                })
                .collect();
            (indices, headings)
        };
        // The managed core's own group is last, and only a managed
        // connection lists it.
        let (indices, titles) = headings(&Setting::ALL);
        assert_eq!(indices, (0..Setting::ALL.len()).collect::<Vec<_>>());
        assert_eq!(titles, ["基础", "端口", "TUN", "维护", "托管内核"]);
        let external: Vec<Setting> = Setting::ALL[..Setting::ALL.len() - 7].to_vec();
        let (indices, titles) = headings(&external);
        assert_eq!(indices, (0..external.len()).collect::<Vec<_>>());
        assert_eq!(titles, ["基础", "端口", "TUN", "维护"]);
    }

    #[test]
    fn a_value_too_wide_is_cut_after_what_fits_whole() {
        let spans = vec![Span::raw("● 开"), Span::raw("  gvisor · FlClash")];
        let texts = |spans: Vec<Span<'static>>| -> Vec<String> {
            spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect()
        };
        assert_eq!(
            texts(fit(spans.clone(), 30)),
            ["● 开", "  gvisor · FlClash"]
        );
        assert_eq!(texts(fit(spans.clone(), 10)), ["● 开", "  gvi…"]);
        assert_eq!(texts(fit(spans.clone(), 4)), ["● 开"]);
        assert_eq!(texts(fit(spans, 3)), ["● …"]);
    }
}
