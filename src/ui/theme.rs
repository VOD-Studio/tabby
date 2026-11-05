use ratatui::style::Color;

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
    /// Failed / timed out / off-limits.
    pub error: Color,
}

impl Theme {
    /// 16-color ANSI palette, so the terminal's own theme shows through.
    pub const TERMINAL: Self = Self {
        fg: Color::Reset,
        muted: Color::DarkGray,
        accent: Color::Blue,
        success: Color::Green,
        error: Color::Red,
    };
}
