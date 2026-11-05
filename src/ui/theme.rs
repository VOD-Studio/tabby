use ratatui::style::Color;

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
    };

    const TOKYO_NIGHT: Self = Self {
        fg: Color::Rgb(0xc0, 0xca, 0xf5),
        muted: Color::Rgb(0x73, 0x7a, 0xa2),
        accent: Color::Rgb(0x7a, 0xa2, 0xf7),
        success: Color::Rgb(0x9e, 0xce, 0x6a),
        warning: Color::Rgb(0xe0, 0xaf, 0x68),
        error: Color::Rgb(0xf7, 0x76, 0x8e),
    };

    const CATPPUCCIN_MOCHA: Self = Self {
        fg: Color::Rgb(0xcd, 0xd6, 0xf4),
        muted: Color::Rgb(0x7f, 0x84, 0x9c),
        accent: Color::Rgb(0x89, 0xb4, 0xfa),
        success: Color::Rgb(0xa6, 0xe3, 0xa1),
        warning: Color::Rgb(0xf9, 0xe2, 0xaf),
        error: Color::Rgb(0xf3, 0x8b, 0xa8),
    };
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
