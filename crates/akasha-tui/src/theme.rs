//! Theme system for the TUI (inspired by ratatui-themes).
//! Palette + markdown styles, cycle with next/prev.

use ratatui::style::{Color, Modifier, Style};

/// Theme palette: semantic colors for UI and markdown.
#[derive(Debug, Clone)]
pub struct ThemePalette {
    pub accent: Color,
    pub muted: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub info: Color,
    pub bg: Color,
    pub fg: Color,
}

/// Available theme names; cycle with next/prev.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
    #[default]
    Dracula,
    Nord,
    CatppuccinMocha,
    TokyoNight,
    GruvboxDark,
    SolarizedDark,
}

#[allow(dead_code)]
impl ThemeName {
    pub fn next(self) -> Self {
        match self {
            Self::Dracula => Self::Nord,
            Self::Nord => Self::CatppuccinMocha,
            Self::CatppuccinMocha => Self::TokyoNight,
            Self::TokyoNight => Self::GruvboxDark,
            Self::GruvboxDark => Self::SolarizedDark,
            Self::SolarizedDark => Self::Dracula,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Dracula => Self::SolarizedDark,
            Self::Nord => Self::Dracula,
            Self::CatppuccinMocha => Self::Nord,
            Self::TokyoNight => Self::CatppuccinMocha,
            Self::GruvboxDark => Self::TokyoNight,
            Self::SolarizedDark => Self::GruvboxDark,
        }
    }

    pub fn all() -> &'static [ThemeName] {
        &[
            ThemeName::Dracula,
            ThemeName::Nord,
            ThemeName::CatppuccinMocha,
            ThemeName::TokyoNight,
            ThemeName::GruvboxDark,
            ThemeName::SolarizedDark,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dracula => "Dracula",
            Self::Nord => "Nord",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::SolarizedDark => "Solarized Dark",
        }
    }
}

fn palette_for(name: ThemeName) -> ThemePalette {
    use ThemeName::*;
    match name {
        Dracula => ThemePalette {
            accent: Color::Rgb(0xbd, 0x93, 0xf9),
            muted: Color::Rgb(0x62, 0x72, 0xa4),
            error: Color::Rgb(0xff, 0x55, 0x55),
            warning: Color::Rgb(0xf1, 0xfa, 0x8c),
            success: Color::Rgb(0x50, 0xfa, 0x7b),
            info: Color::Rgb(0x8b, 0xe9, 0xfd),
            bg: Color::Rgb(0x28, 0x2a, 0x36),
            fg: Color::Rgb(0xf8, 0xf8, 0xf2),
        },
        Nord => ThemePalette {
            accent: Color::Rgb(0x88, 0xc0, 0xd0),
            muted: Color::Rgb(0x4c, 0x56, 0x6a),
            error: Color::Rgb(0xbf, 0x61, 0x6a),
            warning: Color::Rgb(0xeb, 0xcb, 0x8b),
            success: Color::Rgb(0xa3, 0xbe, 0x8c),
            info: Color::Rgb(0x81, 0xa1, 0xc1),
            bg: Color::Rgb(0x2e, 0x34, 0x40),
            fg: Color::Rgb(0xec, 0xef, 0xf4),
        },
        CatppuccinMocha => ThemePalette {
            accent: Color::Rgb(0xcb, 0xa6, 0xf7),
            muted: Color::Rgb(0x6c, 0x70, 0x80),
            error: Color::Rgb(0xf3, 0x8b, 0xa8),
            warning: Color::Rgb(0xfa, 0xe3, 0xb0),
            success: Color::Rgb(0xa6, 0xe3, 0xa1),
            info: Color::Rgb(0x89, 0xb4, 0xfa),
            bg: Color::Rgb(0x1e, 0x1e, 0x2e),
            fg: Color::Rgb(0xcd, 0xd6, 0xf4),
        },
        TokyoNight => ThemePalette {
            accent: Color::Rgb(0x7a, 0xa2, 0xf7),
            muted: Color::Rgb(0x56, 0x5f, 0x89),
            error: Color::Rgb(0xf7, 0x76, 0x8e),
            warning: Color::Rgb(0xe0, 0xaf, 0x68),
            success: Color::Rgb(0x9e, 0xce, 0x6a),
            info: Color::Rgb(0x7d, 0xcf, 0xf1),
            bg: Color::Rgb(0x1a, 0x1b, 0x26),
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
        },
        GruvboxDark => ThemePalette {
            accent: Color::Rgb(0xfe, 0x80, 0x19),
            muted: Color::Rgb(0x92, 0x83, 0x74),
            error: Color::Rgb(0xfb, 0x49, 0x34),
            warning: Color::Rgb(0xfa, 0xbd, 0x2f),
            success: Color::Rgb(0xb8, 0xbb, 0x26),
            info: Color::Rgb(0x83, 0xa5, 0x98),
            bg: Color::Rgb(0x28, 0x28, 0x28),
            fg: Color::Rgb(0xeb, 0xdb, 0xb2),
        },
        SolarizedDark => ThemePalette {
            accent: Color::Rgb(0x26, 0x8b, 0xd2),
            muted: Color::Rgb(0x58, 0x6e, 0x75),
            error: Color::Rgb(0xdc, 0x32, 0x2f),
            warning: Color::Rgb(0xb5, 0x89, 0x00),
            success: Color::Rgb(0x85, 0x99, 0x00),
            info: Color::Rgb(0x2a, 0xa1, 0x98),
            bg: Color::Rgb(0x00, 0x2b, 0x36),
            fg: Color::Rgb(0x83, 0x94, 0x96),
        },
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // name kept for API (e.g. theme picker)
pub struct Theme {
    pub name: ThemeName,
    pub palette: ThemePalette,
}

impl Theme {
    pub fn new(name: ThemeName) -> Self {
        let palette = palette_for(name);
        Self { name, palette }
    }

    pub fn palette(&self) -> &ThemePalette {
        &self.palette
    }

    /// Styles for blocks, tabs, borders.
    pub fn block_border(&self) -> Style {
        Style::default().fg(self.palette.accent)
    }

    pub fn tab_active(&self) -> Style {
        Style::default()
            .fg(self.palette.bg)
            .bg(self.palette.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn tab_inactive(&self) -> Style {
        Style::default().fg(self.palette.muted)
    }

    pub fn markdown_styles(&self) -> crate::markdown::MarkdownStyles {
        crate::markdown::MarkdownStyles::from_palette(&self.palette)
    }
}
