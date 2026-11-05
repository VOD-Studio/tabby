use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    api::models::CoreKind,
    app::{App, Connection},
    format,
};

const NAME: &str = " tabby";
/// Between the logo, the kernel badge and the mode pill, and between the
/// rates and the status dot.
const SEPARATOR: &str = "  ";
/// Columns kept clear between the two sides.
const GAP: usize = 2;
/// `↑ 1023.9 KB/s`: arrow, number and unit as [`rate`] pads them. The
/// arrow, like the logo mark and the status dot, is an icon: one column.
const RATE_WIDTH: usize = 13;

/// The parts of the top bar that come and go, in the order they go as it
/// narrows. The logo mark and the status dot are always there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Profile,
    /// `tabby` beside the logo mark.
    Name,
    /// The kernel badge.
    Core,
    Up,
    Down,
    Mode,
}

const DROP_ORDER: [Part; 6] = [
    Part::Profile,
    Part::Name,
    Part::Core,
    Part::Up,
    Part::Down,
    Part::Mode,
];

/// How wide the parts that come and go are: `core` is the kernel badge
/// (`mihomo v1.19.33`) and `mode` the pill (` RULE `), 0 when there is
/// none; `traffic` whether there are rates to show; `profile` the
/// profile's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Widths {
    core: usize,
    mode: usize,
    traffic: bool,
    profile: usize,
}

/// What the top bar shows: the parts it has, less the first `dropped` of
/// [`DROP_ORDER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    dropped: usize,
    widths: Widths,
}

impl Shown {
    fn has(self, part: Part) -> bool {
        let there = match part {
            Part::Core => self.widths.core > 0,
            Part::Mode => self.widths.mode > 0,
            Part::Up | Part::Down => self.widths.traffic,
            Part::Profile | Part::Name => true,
        };
        there && !DROP_ORDER[..self.dropped].contains(&part)
    }

    /// Columns the parts take, gap between the sides included.
    fn width(self) -> usize {
        let widths = self.widths;
        let part = |part: Part, width: usize| if self.has(part) { width } else { 0 };
        // The logo mark: one column, as every icon is.
        let left = 1
            + part(Part::Name, NAME.width())
            + part(Part::Core, SEPARATOR.len() + widths.core)
            + part(Part::Mode, SEPARATOR.len() + widths.mode);
        let right = part(Part::Up, RATE_WIDTH + SEPARATOR.len())
            + part(Part::Down, RATE_WIDTH + SEPARATOR.len())
            + 1
            + part(Part::Profile, 1 + widths.profile);
        left + GAP + right
    }
}

/// What of the top bar fits in `width` columns: parts go in
/// [`DROP_ORDER`] until the rest fits.
fn plan(width: usize, widths: Widths) -> Shown {
    (0..DROP_ORDER.len())
        .map(|dropped| Shown { dropped, widths })
        .find(|shown| shown.width() <= width)
        .unwrap_or(Shown {
            dropped: DROP_ORDER.len(),
            widths,
        })
}

/// One-line top bar: logo, kernel badge, mode pill; rates, connection dot
/// and profile name on the right. Parts that do not fit are left out by
/// [`plan`] rather than drawn over each other.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let bold = Modifier::BOLD;

    let core = match &app.version {
        Some(Ok(version)) => {
            let core = match version.kind() {
                CoreKind::Mihomo => "mihomo",
                CoreKind::Clash => "clash",
            };
            format!("{core} {}", version.version)
        }
        _ => String::new(),
    };
    let mode = match &app.configs {
        Some(Ok(configs)) if !configs.mode.is_empty() => {
            // A mode tabby does not know is shown as the kernel spells it.
            let tag = configs
                .mode()
                .map_or_else(|| configs.mode.clone(), |mode| mode.name().to_uppercase());
            format!(" {tag} ")
        }
        _ => String::new(),
    };
    let profile = &app.settings.profile;
    let shown = plan(
        usize::from(area.width),
        Widths {
            core: core.width(),
            mode: mode.width(),
            traffic: app.traffic.is_some(),
            profile: profile.width(),
        },
    );

    let accent = Style::new().fg(theme.accent).add_modifier(bold);
    let icons = &app.icons;
    let mut left = vec![Span::styled(icons.logo, accent)];
    if shown.has(Part::Name) {
        left.push(Span::styled(NAME, accent));
    }
    if shown.has(Part::Core) {
        left.push(Span::raw(SEPARATOR));
        left.push(Span::styled(core, Style::new().fg(theme.muted)));
    }
    if shown.has(Part::Mode) {
        left.push(Span::raw(SEPARATOR));
        // Reversed, so the pill is accent-filled in any palette.
        left.push(Span::styled(mode, accent.add_modifier(Modifier::REVERSED)));
    }

    let dot = match app.connection() {
        Connection::Connecting => theme.muted,
        Connection::Connected => theme.success,
        Connection::Disconnected => theme.error,
    };
    let mut right = vec![];
    if let Some(traffic) = app.traffic {
        if shown.has(Part::Up) {
            right.extend(rate(theme.c1, icons.upload, traffic.up, theme.muted));
            right.push(Span::raw(SEPARATOR));
        }
        if shown.has(Part::Down) {
            right.extend(rate(theme.c2, icons.download, traffic.down, theme.muted));
            right.push(Span::raw(SEPARATOR));
        }
    }
    right.push(Span::styled(icons.dot, Style::new().fg(dot)));
    if shown.has(Part::Profile) {
        right.push(Span::styled(
            format!(" {profile}"),
            Style::new().fg(theme.fg),
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `mihomo v1.19.33`, ` RULE `, rates, profile `flclash`.
    const FULL: Widths = Widths {
        core: 15,
        mode: 6,
        traffic: true,
        profile: 7,
    };

    #[test]
    fn rates_take_the_width_they_are_given() {
        let spans = rate(Color::Reset, "↑", 1_048_525, Color::Reset);
        let width: usize = spans.iter().map(Span::width).sum();
        assert_eq!(width, RATE_WIDTH);
    }

    /// The parts shown at `width`, in [`DROP_ORDER`].
    fn parts(width: usize, widths: Widths) -> Vec<Part> {
        let shown = plan(width, widths);
        DROP_ORDER
            .into_iter()
            .filter(|&part| shown.has(part))
            .collect()
    }

    #[test]
    fn parts_go_in_order_as_the_bar_narrows() {
        use Part::{Core, Down, Mode, Name, Profile, Up};
        let parts = |width| parts(width, FULL);
        // Left 7 + 17 + 8, gap 2, right 15 + 15 + 1 + 8.
        assert_eq!(parts(73), [Profile, Name, Core, Up, Down, Mode]);
        assert_eq!(parts(72), [Name, Core, Up, Down, Mode]);
        assert_eq!(parts(65), [Name, Core, Up, Down, Mode]);
        assert_eq!(parts(64), [Core, Up, Down, Mode]);
        assert_eq!(parts(59), [Core, Up, Down, Mode]);
        assert_eq!(parts(58), [Up, Down, Mode]);
        // The narrowest terminal drawn into keeps the mode and both rates.
        assert_eq!(parts(50), [Up, Down, Mode]);
        assert_eq!(parts(42), [Up, Down, Mode]);
        assert_eq!(parts(41), [Down, Mode]);
        assert_eq!(parts(27), [Down, Mode]);
        assert_eq!(parts(26), [Mode]);
        assert_eq!(parts(12), [Mode]);
        assert_eq!(parts(11), Vec::<Part>::new());
    }

    #[test]
    fn missing_parts_leave_room_for_the_rest() {
        use Part::{Down, Mode, Name, Profile, Up};
        // No `/version`, `/configs` or traffic yet: logo, name, dot and
        // profile.
        let bare = Widths {
            core: 0,
            mode: 0,
            traffic: false,
            profile: 7,
        };
        assert_eq!(parts(50, bare), [Profile, Name]);
        // A mode name the kernel made up, 20 columns: the kernel badge
        // goes sooner, the rates stay.
        let long_mode = Widths { mode: 20, ..FULL };
        assert_eq!(parts(70, long_mode), [Up, Down, Mode]);
    }
}
