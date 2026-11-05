use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    api::models::CoreKind,
    app::{App, Connection},
    format,
};

/// One-line top bar: logo, kernel badge, mode pill; connection dot and profile
/// name on the right.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let bold = Modifier::BOLD;

    let mut left = vec![Span::styled(
        "◆ tabby",
        Style::new().fg(theme.accent).add_modifier(bold),
    )];
    if let Some(Ok(version)) = &app.version {
        let core = match version.kind() {
            CoreKind::Mihomo => "mihomo",
            CoreKind::Clash => "clash",
        };
        left.push(Span::styled(
            format!("  {core} {}", version.version),
            Style::new().fg(theme.muted),
        ));
    }
    if let Some(Ok(configs)) = &app.configs
        && !configs.mode.is_empty()
    {
        // A mode tabby does not know is shown as the kernel spells it.
        let tag = configs
            .mode()
            .map_or_else(|| configs.mode.clone(), |mode| mode.name().to_uppercase());
        left.push(Span::raw("  "));
        // Reversed, so the pill is accent-filled in any palette.
        left.push(Span::styled(
            format!(" {tag} "),
            Style::new()
                .fg(theme.accent)
                .add_modifier(bold | Modifier::REVERSED),
        ));
    }

    let dot = match app.connection() {
        Connection::Connecting => theme.muted,
        Connection::Connected => theme.success,
        Connection::Disconnected => theme.error,
    };
    let mut right = vec![];
    if let Some(traffic) = app.traffic {
        right.extend(rate(theme.c1, "↑", traffic.up, theme.muted));
        right.push(Span::raw("  "));
        right.extend(rate(theme.c2, "↓", traffic.down, theme.muted));
        right.push(Span::raw("  "));
    }
    right.push(Span::styled("●", Style::new().fg(dot)));
    right.push(Span::styled(
        format!(" {}", app.settings.profile),
        Style::new().fg(theme.fg),
    ));
    let right = Line::from(right);

    let right_width = u16::try_from(right.width()).unwrap_or(u16::MAX);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(left)), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}

/// `↑   1.2 MB/s`: the arrow and number in `color`, the unit in `muted`.
///
/// The number is right-aligned in the widest width it takes below 1024, and
/// the unit padded to the longest one, so the line keeps its width as the
/// rate swings and the status dot beside it does not jitter.
fn rate(
    color: Color,
    arrow: &'static str,
    bytes_per_second: u64,
    muted: Color,
) -> [Span<'static>; 2] {
    let (number, unit) = format::rate(bytes_per_second);
    [
        Span::styled(format!("{arrow} {number:>6}"), Style::new().fg(color)),
        Span::styled(format!(" {unit:<4}"), Style::new().fg(muted)),
    ]
}
