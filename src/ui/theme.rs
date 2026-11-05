use ratatui::style::{Color, Modifier, Style};

use crate::config::ThemeName;

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
    /// Cursor row background. A style rather than a color: the `terminal`
    /// theme has no background of its own to pick and inverts instead.
    pub surface: Style,
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
        surface: Style::new().add_modifier(Modifier::REVERSED),
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
        surface: Style::new().bg(Color::Rgb(0x29, 0x2e, 0x42)),
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
        surface: Style::new().bg(Color::Rgb(0x31, 0x32, 0x44)),
    };

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
