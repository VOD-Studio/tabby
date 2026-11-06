use ratatui::style::{Color, Modifier, Style};

use crate::config::{ThemeName, ThemeOverrides};

/// Semantic colors. UI code refers to these roles, never to concrete colors.
#[derive(Debug, Clone, Copy)]
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
    /// theme has no background of its own to pick and inverts instead.
    pub surface: Style,
    /// Background of every other table row. Between the page background and
    /// `surface`, so the cursor row still stands out. The `terminal` theme
    /// has none: no 16-color background is faint enough to leave muted text
    /// readable.
    pub stripe: Style,
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
        surface: Style::new().add_modifier(Modifier::REVERSED),
        stripe: Style::new(),
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
    };

    /// This theme with the colors `overrides` name replaced.
    ///
    /// `surface` is a background, not a color of text: it replaces the whole
    /// style, so on the `terminal` theme it takes the place of the reverse
    /// video (the cursor row then keeps its text colors). `stripe` has no
    /// key and stays.
    pub fn with_overrides(mut self, overrides: &ThemeOverrides) -> Self {
        // Exhaustive, so a color added to the struct cannot be left out.
        let ThemeOverrides {
            fg,
            muted,
            surface,
            accent,
            success,
            warning,
            error,
            c1,
            c2,
            c3,
            c4,
        } = *overrides;
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
        if let Some(color) = surface {
            self.surface = Style::new().bg(color);
        }
        self
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEMES: [ThemeName; 3] = [
        ThemeName::Terminal,
        ThemeName::TokyoNight,
        ThemeName::CatppuccinMocha,
    ];

    /// Every color of `theme`, then its two background styles.
    fn parts(theme: &Theme) -> ([Color; 10], Style, Style) {
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
        } = *theme;
        (
            [fg, muted, accent, success, warning, error, c1, c2, c3, c4],
            surface,
            stripe,
        )
    }

    /// Not any color a theme has.
    const MARK: Color = Color::Rgb(1, 2, 3);

    #[test]
    fn an_override_changes_its_own_color_and_no_other_on_every_theme() {
        // In the order of `parts`.
        let each: [fn(&mut ThemeOverrides); 10] = [
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
        ];
        for name in THEMES {
            let (colors, surface, stripe) = parts(&Theme::from(name));
            let untouched = Theme::from(name).with_overrides(&ThemeOverrides::default());
            assert_eq!(parts(&untouched), (colors, surface, stripe), "{name:?}");
            for (index, set) in each.into_iter().enumerate() {
                let mut overrides = ThemeOverrides::default();
                set(&mut overrides);
                let theme = Theme::from(name).with_overrides(&overrides);
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
        let overrides = ThemeOverrides {
            surface: Some(MARK),
            ..ThemeOverrides::default()
        };
        for name in THEMES {
            let base = Theme::from(name);
            let theme = base.with_overrides(&overrides);
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
}
