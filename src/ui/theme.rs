use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::config::{Palette, ThemeName};

/// Semantic colors. UI code refers to these roles, never to concrete colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Body text.
    pub fg: Color,
    /// Secondary text, borders, units.
    pub muted: Color,
    /// Focus, selection, active tab, key names in hints.
    pub accent: Color,
    /// Healthy / connected / on.
    pub success: Color,
    /// Degraded / needs attention: reconnecting.
    pub warning: Color,
    /// Failed / timed out / off-limits.
    pub error: Color,
    /// Upload, everywhere it is shown.
    pub c1: Color,
    /// Download, everywhere it is shown.
    pub c2: Color,
    /// Open connections, everywhere they are shown.
    pub c3: Color,
    /// Memory, everywhere it is shown.
    pub c4: Color,
    /// Cursor row background. A style rather than a color: the `terminal`
    /// theme has no background of its own to pick and inverts instead. It
    /// goes on top of a drawn row, through [`Theme::paint_cursor`] or a
    /// table's row highlight, never under it.
    pub surface: Style,
    /// Background of every other table row. Between the page background and
    /// `surface`, so the cursor row still stands out. The `terminal` theme
    /// has none: no 16-color background is faint enough to leave muted text
    /// readable.
    pub stripe: Style,
    /// Background of the whole screen, laid under every cell left on the
    /// terminal's own (`Reset`), popups included. `Reset` keeps the
    /// terminal's background, as the dark built-in themes do.
    pub bg: Color,
}

impl Theme {
    /// 16-color ANSI palette, so the terminal's own theme shows through.
    const TERMINAL: Self = Self {
        fg: Color::Reset,
        muted: Color::DarkGray,
        accent: Color::Blue,
        success: Color::Green,
        warning: Color::Yellow,
        error: Color::Red,
        c1: Color::Magenta,
        c2: Color::Cyan,
        c3: Color::LightRed,
        c4: Color::LightCyan,
        // Default colors, so every cell of the row inverts to the same two:
        // left with their own, the row's colors would turn into backgrounds.
        surface: Style::new()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED),
        stripe: Style::new(),
        bg: Color::Reset,
    };

    const TOKYO_NIGHT: Self = Self {
        fg: Color::Rgb(0xc0, 0xca, 0xf5),
        muted: Color::Rgb(0x73, 0x7a, 0xa2),
        accent: Color::Rgb(0x7a, 0xa2, 0xf7),
        success: Color::Rgb(0x9e, 0xce, 0x6a),
        warning: Color::Rgb(0xe0, 0xaf, 0x68),
        error: Color::Rgb(0xf7, 0x76, 0x8e),
        c1: Color::Rgb(0xbb, 0x9a, 0xf7),
        c2: Color::Rgb(0x7d, 0xcf, 0xff),
        c3: Color::Rgb(0xff, 0x9e, 0x64),
        c4: Color::Rgb(0x73, 0xda, 0xca),
        surface: Style::new().bg(Color::Rgb(0x29, 0x2e, 0x42)),
        stripe: Style::new().bg(Color::Rgb(0x21, 0x24, 0x34)),
        bg: Color::Reset,
    };

    const CATPPUCCIN_MOCHA: Self = Self {
        fg: Color::Rgb(0xcd, 0xd6, 0xf4),
        muted: Color::Rgb(0x7f, 0x84, 0x9c),
        accent: Color::Rgb(0x89, 0xb4, 0xfa),
        success: Color::Rgb(0xa6, 0xe3, 0xa1),
        warning: Color::Rgb(0xf9, 0xe2, 0xaf),
        error: Color::Rgb(0xf3, 0x8b, 0xa8),
        c1: Color::Rgb(0xcb, 0xa6, 0xf7),
        c2: Color::Rgb(0x89, 0xdc, 0xeb),
        c3: Color::Rgb(0xfa, 0xb3, 0x87),
        c4: Color::Rgb(0x94, 0xe2, 0xd5),
        surface: Style::new().bg(Color::Rgb(0x31, 0x32, 0x44)),
        stripe: Style::new().bg(Color::Rgb(0x27, 0x26, 0x39)),
        bg: Color::Reset,
    };

    /// Light: it brings its own background, as dark text on the terminal's
    /// would be unreadable on a dark one.
    const CATPPUCCIN_LATTE: Self = Self {
        fg: Color::Rgb(0x4c, 0x4f, 0x69),
        muted: Color::Rgb(0x8c, 0x8f, 0xa1),
        accent: Color::Rgb(0x1e, 0x66, 0xf5),
        success: Color::Rgb(0x40, 0xa0, 0x2b),
        warning: Color::Rgb(0xdf, 0x8e, 0x1d),
        error: Color::Rgb(0xd2, 0x0f, 0x39),
        c1: Color::Rgb(0x88, 0x39, 0xef),
        c2: Color::Rgb(0x04, 0xa5, 0xe5),
        c3: Color::Rgb(0xfe, 0x64, 0x0b),
        c4: Color::Rgb(0x17, 0x92, 0x99),
        surface: Style::new().bg(Color::Rgb(0xcc, 0xd0, 0xda)),
        stripe: Style::new().bg(Color::Rgb(0xdd, 0xe0, 0xe7)),
        bg: Color::Rgb(0xef, 0xf1, 0xf5),
    };

    const GRUVBOX_DARK: Self = Self {
        fg: Color::Rgb(0xeb, 0xdb, 0xb2),
        muted: Color::Rgb(0x92, 0x83, 0x74),
        accent: Color::Rgb(0x83, 0xa5, 0x98),
        success: Color::Rgb(0xb8, 0xbb, 0x26),
        warning: Color::Rgb(0xfa, 0xbd, 0x2f),
        error: Color::Rgb(0xfb, 0x49, 0x34),
        c1: Color::Rgb(0xd3, 0x86, 0x9b),
        c2: Color::Rgb(0x8e, 0xc0, 0x7c),
        c3: Color::Rgb(0xfe, 0x80, 0x19),
        c4: Color::Rgb(0x45, 0x85, 0x88),
        surface: Style::new().bg(Color::Rgb(0x3c, 0x38, 0x36)),
        stripe: Style::new().bg(Color::Rgb(0x32, 0x30, 0x2f)),
        bg: Color::Reset,
    };

    const NORD: Self = Self {
        fg: Color::Rgb(0xd8, 0xde, 0xe9),
        muted: Color::Rgb(0x61, 0x6e, 0x88),
        accent: Color::Rgb(0x88, 0xc0, 0xd0),
        success: Color::Rgb(0xa3, 0xbe, 0x8c),
        warning: Color::Rgb(0xeb, 0xcb, 0x8b),
        error: Color::Rgb(0xbf, 0x61, 0x6a),
        c1: Color::Rgb(0xb4, 0x8e, 0xad),
        c2: Color::Rgb(0x81, 0xa1, 0xc1),
        c3: Color::Rgb(0xd0, 0x87, 0x70),
        c4: Color::Rgb(0x8f, 0xbc, 0xbb),
        surface: Style::new().bg(Color::Rgb(0x3b, 0x42, 0x52)),
        stripe: Style::new().bg(Color::Rgb(0x34, 0x3b, 0x49)),
        bg: Color::Reset,
    };

    const DRACULA: Self = Self {
        fg: Color::Rgb(0xf8, 0xf8, 0xf2),
        muted: Color::Rgb(0x62, 0x72, 0xa4),
        accent: Color::Rgb(0xbd, 0x93, 0xf9),
        success: Color::Rgb(0x50, 0xfa, 0x7b),
        warning: Color::Rgb(0xf1, 0xfa, 0x8c),
        error: Color::Rgb(0xff, 0x55, 0x55),
        c1: Color::Rgb(0xff, 0x79, 0xc6),
        c2: Color::Rgb(0x8b, 0xe9, 0xfd),
        c3: Color::Rgb(0xff, 0xb8, 0x6c),
        c4: Color::Rgb(0xa4, 0xff, 0xff),
        surface: Style::new().bg(Color::Rgb(0x44, 0x47, 0x5a)),
        stripe: Style::new().bg(Color::Rgb(0x36, 0x38, 0x48)),
        bg: Color::Reset,
    };

    /// This theme with the colors `palette` names replaced.
    ///
    /// `surface` and `stripe` are backgrounds, not colors of text: each
    /// replaces the whole style, so on the `terminal` theme `surface` takes
    /// the place of the reverse video (the cursor row then keeps its text
    /// colors). A `stripe` of `Reset` means no stripes.
    pub fn with(mut self, palette: &Palette) -> Self {
        // Exhaustive, so a color added to the struct cannot be left out.
        let Palette {
            fg,
            muted,
            surface,
            stripe,
            bg,
            accent,
            success,
            warning,
            error,
            c1,
            c2,
            c3,
            c4,
        } = *palette;
        self.fg = fg.unwrap_or(self.fg);
        self.muted = muted.unwrap_or(self.muted);
        self.accent = accent.unwrap_or(self.accent);
        self.success = success.unwrap_or(self.success);
        self.warning = warning.unwrap_or(self.warning);
        self.error = error.unwrap_or(self.error);
        self.c1 = c1.unwrap_or(self.c1);
        self.c2 = c2.unwrap_or(self.c2);
        self.c3 = c3.unwrap_or(self.c3);
        self.c4 = c4.unwrap_or(self.c4);
        self.bg = bg.unwrap_or(self.bg);
        if let Some(color) = surface {
            self.surface = Style::new().bg(color);
        }
        match stripe {
            Some(Color::Reset) => self.stripe = Style::new(),
            Some(color) => self.stripe = Style::new().bg(color),
            None => {}
        }
        self
    }

    /// Gives `area` of `buf`, already drawn, the cursor's look.
    ///
    /// Laid on top rather than under, so the `terminal` theme's reverse video
    /// turns the whole area into one pair of colors: under the text it would
    /// invert each span's own color into a background of its own. The other
    /// themes only change the background, and the text keeps its colors.
    pub fn paint_cursor(&self, buf: &mut Buffer, area: Rect) {
        buf.set_style(area, self.surface);
    }

    /// Color for a delay test result: under 200 ms good, up to 500 ms slow,
    /// beyond that bad.
    pub fn delay(&self, millis: u32) -> Color {
        match millis {
            0..200 => self.success,
            200..=500 => self.warning,
            _ => self.error,
        }
    }
}

impl From<ThemeName> for Theme {
    fn from(name: ThemeName) -> Self {
        match name {
            ThemeName::Terminal => Self::TERMINAL,
            ThemeName::TokyoNight => Self::TOKYO_NIGHT,
            ThemeName::CatppuccinMocha => Self::CATPPUCCIN_MOCHA,
            ThemeName::CatppuccinLatte => Self::CATPPUCCIN_LATTE,
            ThemeName::GruvboxDark => Self::GRUVBOX_DARK,
            ThemeName::Nord => Self::NORD,
            ThemeName::Dracula => Self::DRACULA,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEMES: [ThemeName; 7] = [
        ThemeName::Terminal,
        ThemeName::TokyoNight,
        ThemeName::CatppuccinMocha,
        ThemeName::CatppuccinLatte,
        ThemeName::GruvboxDark,
        ThemeName::Nord,
        ThemeName::Dracula,
    ];

    /// Every color of `theme`, then its two background styles.
    fn parts(theme: &Theme) -> ([Color; 11], Style, Style) {
        let Theme {
            fg,
            muted,
            accent,
            success,
            warning,
            error,
            c1,
            c2,
            c3,
            c4,
            surface,
            stripe,
            bg,
        } = *theme;
        (
            [
                fg, muted, accent, success, warning, error, c1, c2, c3, c4, bg,
            ],
            surface,
            stripe,
        )
    }

    /// Not any color a theme has.
    const MARK: Color = Color::Rgb(1, 2, 3);

    #[test]
    fn an_override_changes_its_own_color_and_no_other_on_every_theme() {
        // In the order of `parts`.
        let each: [fn(&mut Palette); 11] = [
            |o| o.fg = Some(MARK),
            |o| o.muted = Some(MARK),
            |o| o.accent = Some(MARK),
            |o| o.success = Some(MARK),
            |o| o.warning = Some(MARK),
            |o| o.error = Some(MARK),
            |o| o.c1 = Some(MARK),
            |o| o.c2 = Some(MARK),
            |o| o.c3 = Some(MARK),
            |o| o.c4 = Some(MARK),
            |o| o.bg = Some(MARK),
        ];
        for name in THEMES {
            let (colors, surface, stripe) = parts(&Theme::from(name));
            let untouched = Theme::from(name).with(&Palette::default());
            assert_eq!(parts(&untouched), (colors, surface, stripe), "{name:?}");
            for (index, set) in each.into_iter().enumerate() {
                let mut palette = Palette::default();
                set(&mut palette);
                let theme = Theme::from(name).with(&palette);
                let mut expected = colors;
                expected[index] = MARK;
                assert_eq!(
                    parts(&theme),
                    (expected, surface, stripe),
                    "{name:?} color {index}"
                );
            }
        }
    }

    #[test]
    fn a_surface_override_is_a_background_that_replaces_the_reverse_video() {
        let palette = Palette {
            surface: Some(MARK),
            ..Palette::default()
        };
        for name in THEMES {
            let base = Theme::from(name);
            let theme = base.with(&palette);
            assert_eq!(theme.surface, Style::new().bg(MARK), "{name:?}");
            // Nothing else moves, the stripe included.
            assert_eq!(theme.stripe, base.stripe, "{name:?}");
            assert_eq!(parts(&theme).0, parts(&base).0, "{name:?}");
        }
        assert!(
            Theme::from(ThemeName::Terminal)
                .surface
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn a_stripe_override_is_a_background_and_reset_removes_the_stripes() {
        let stripe = |color| {
            let palette = Palette {
                stripe: Some(color),
                ..Palette::default()
            };
            Theme::from(ThemeName::TokyoNight).with(&palette)
        };
        let base = Theme::from(ThemeName::TokyoNight);
        let theme = stripe(MARK);
        assert_eq!(theme.stripe, Style::new().bg(MARK));
        assert_eq!(
            (theme.surface, parts(&theme).0),
            (base.surface, parts(&base).0)
        );
        assert_eq!(stripe(Color::Reset).stripe, Style::new());
    }
}
