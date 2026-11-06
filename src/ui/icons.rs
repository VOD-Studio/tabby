use crate::{app::Page, config::IconSet};

/// Symbols that stand for something: states, switches, marks, the logo, the
/// tabs (plan §7.2). UI code takes these from here and never writes them
/// out. Typography and data visualization (`…`, the sort arrows `▲` `▼`,
/// the route arrow `→`, the scroll marks `▴` `▾`, block and box-drawing
/// characters, the braille spinner) look the same in both sets and stay
/// where they are drawn.
///
/// Every icon is one column wide as `unicode-width` measures it, so the
/// layout is the same in both sets.
#[derive(Debug, Clone, Copy)]
pub struct Icons {
    /// The logo mark at the left of the top bar.
    pub logo: &'static str,
    /// The connection dot in the top bar, `实时` streams, the chart legend.
    pub dot: &'static str,
    /// A stream that is down.
    pub down: &'static str,
    /// A stream on its way up.
    pub connecting: &'static str,
    /// A table or log that has stopped following its stream.
    pub paused: &'static str,
    /// A setting that is on.
    pub on: &'static str,
    /// A setting that is off.
    pub off: &'static str,
    /// The current one of a list: node, group, profile; a success toast.
    pub check: &'static str,
    /// A failure: error state, error toast, a rejected secret.
    pub cross: &'static str,
    /// The reconnecting banner.
    pub warning: &'static str,
    /// A list with nothing in it.
    pub empty: &'static str,
    /// Upload rate in the top bar.
    pub upload: &'static str,
    /// Download rate in the top bar.
    pub download: &'static str,
    /// Before each tab's title, in [`Page::ALL`] order; `None` leaves the
    /// tabs as numbers and titles.
    pub pages: Option<[&'static str; Page::ALL.len()]>,
}

impl Icons {
    const UNICODE: Self = Self {
        logo: "◆",
        dot: "●",
        down: "○",
        connecting: "◌",
        paused: "⏸",
        on: "●",
        off: "○",
        check: "✓",
        cross: "✗",
        warning: "⚠",
        empty: "○",
        upload: "↑",
        download: "↓",
        pages: None,
    };

    /// Nerd Fonts v3 code points, Material Design (`nf-md-*`) only.
    const NERD: Self = Self {
        logo: "\u{f011b}",       // nf-md-cat
        dot: "\u{f0765}",        // nf-md-circle
        down: "\u{f0766}",       // nf-md-circle_outline
        connecting: "\u{f1978}", // nf-md-dots_circle
        paused: "\u{f03e4}",     // nf-md-pause
        on: "\u{f0521}",         // nf-md-toggle_switch
        off: "\u{f0a19}",        // nf-md-toggle_switch_off_outline
        check: "\u{f012c}",      // nf-md-check
        cross: "\u{f0156}",      // nf-md-close
        warning: "\u{f0026}",    // nf-md-alert
        empty: "\u{f1274}",      // nf-md-inbox_outline
        upload: "\u{f0737}",     // nf-md-arrow_up_bold
        download: "\u{f072e}",   // nf-md-arrow_down_bold
        pages: Some([
            "\u{f056e}", // Overview: nf-md-view_dashboard
            "\u{f048d}", // Proxies: nf-md-server_network
            "\u{f03d6}", // Providers: nf-md-package_variant
            "\u{f0756}", // Rules: nf-md-format_list_checks
            "\u{f0318}", // Connections: nf-md-lan_connect
            "\u{f09ed}", // Logs: nf-md-text_box_outline
            "\u{f0493}", // Settings: nf-md-cog
            "\u{f046a}", // Subscriptions: nf-md-rss
        ]),
    };
}

impl From<IconSet> for Icons {
    fn from(set: IconSet) -> Self {
        match set {
            IconSet::Unicode => Self::UNICODE,
            IconSet::Nerd => Self::NERD,
        }
    }
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    #[test]
    fn every_icon_is_one_character_one_column_wide() {
        for set in [IconSet::Unicode, IconSet::Nerd] {
            // Destructured, so an icon added later cannot miss the check.
            let Icons {
                logo,
                dot,
                down,
                connecting,
                paused,
                on,
                off,
                check,
                cross,
                warning,
                empty,
                upload,
                download,
                pages,
            } = Icons::from(set);
            let icons = [
                logo, dot, down, connecting, paused, on, off, check, cross, warning, empty, upload,
                download,
            ];
            for icon in icons.into_iter().chain(pages.into_iter().flatten()) {
                assert_eq!(icon.chars().count(), 1, "{set:?} {icon:?}");
                assert_eq!(icon.width(), 1, "{set:?} {icon:?}");
            }
        }
    }
}
