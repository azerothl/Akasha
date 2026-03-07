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

/// Available theme names; cycle with next/prev. Includes dark and light themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
    #[default]
    Dracula,
    Nord,
    CatppuccinMocha,
    TokyoNight,
    GruvboxDark,
    SolarizedDark,
    /// Light themes
    SolarizedLight,
    GruvboxLight,
    CatppuccinLatte,
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
            Self::SolarizedDark => Self::SolarizedLight,
            Self::SolarizedLight => Self::GruvboxLight,
            Self::GruvboxLight => Self::CatppuccinLatte,
            Self::CatppuccinLatte => Self::Dracula,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Dracula => Self::CatppuccinLatte,
            Self::Nord => Self::Dracula,
            Self::CatppuccinMocha => Self::Nord,
            Self::TokyoNight => Self::CatppuccinMocha,
            Self::GruvboxDark => Self::TokyoNight,
            Self::SolarizedDark => Self::GruvboxDark,
            Self::SolarizedLight => Self::SolarizedDark,
            Self::GruvboxLight => Self::SolarizedLight,
            Self::CatppuccinLatte => Self::GruvboxLight,
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
            ThemeName::SolarizedLight,
            ThemeName::GruvboxLight,
            ThemeName::CatppuccinLatte,
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
            Self::SolarizedLight => "Solarized Light",
            Self::GruvboxLight => "Gruvbox Light",
            Self::CatppuccinLatte => "Catppuccin Latte",
        }
    }

    /// Parse from saved string (e.g. "dracula", "solarized_light").
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "dracula" => Some(Self::Dracula),
            "nord" => Some(Self::Nord),
            "catppuccin_mocha" | "mocha" => Some(Self::CatppuccinMocha),
            "tokyo_night" | "tokyonight" => Some(Self::TokyoNight),
            "gruvbox_dark" | "gruvbox" => Some(Self::GruvboxDark),
            "solarized_dark" => Some(Self::SolarizedDark),
            "solarized_light" => Some(Self::SolarizedLight),
            "gruvbox_light" => Some(Self::GruvboxLight),
            "catppuccin_latte" | "latte" => Some(Self::CatppuccinLatte),
            _ => None,
        }
    }

    pub fn to_saved_str(self) -> &'static str {
        match self {
            Self::Dracula => "dracula",
            Self::Nord => "nord",
            Self::CatppuccinMocha => "catppuccin_mocha",
            Self::TokyoNight => "tokyo_night",
            Self::GruvboxDark => "gruvbox_dark",
            Self::SolarizedDark => "solarized_dark",
            Self::SolarizedLight => "solarized_light",
            Self::GruvboxLight => "gruvbox_light",
            Self::CatppuccinLatte => "catppuccin_latte",
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
        SolarizedLight => ThemePalette {
            accent: Color::Rgb(0x26, 0x8b, 0xd2),
            muted: Color::Rgb(0x93, 0xa1, 0xa1),
            error: Color::Rgb(0xdc, 0x32, 0x2f),
            warning: Color::Rgb(0xb5, 0x89, 0x00),
            success: Color::Rgb(0x85, 0x99, 0x00),
            info: Color::Rgb(0x2a, 0xa1, 0x98),
            bg: Color::Rgb(0xfd, 0xf6, 0xe3),
            fg: Color::Rgb(0x65, 0x7b, 0x83),
        },
        GruvboxLight => ThemePalette {
            accent: Color::Rgb(0x42, 0x7b, 0x58),
            muted: Color::Rgb(0x7c, 0x6f, 0x64),
            error: Color::Rgb(0x9d, 0x00, 0x06),
            warning: Color::Rgb(0xb5, 0x76, 0x14),
            success: Color::Rgb(0x79, 0x7c, 0x0e),
            info: Color::Rgb(0x07, 0x66, 0x78),
            bg: Color::Rgb(0xfb, 0xf1, 0xc7),
            fg: Color::Rgb(0x3c, 0x38, 0x36),
        },
        CatppuccinLatte => ThemePalette {
            accent: Color::Rgb(0x88, 0x39, 0xee),
            muted: Color::Rgb(0x6c, 0x6f, 0x85),
            error: Color::Rgb(0xd2, 0x0f, 0x39),
            warning: Color::Rgb(0xdf, 0x8e, 0x1d),
            success: Color::Rgb(0x40, 0xa0, 0x2b),
            info: Color::Rgb(0x1e, 0x66, 0xf5),
            bg: Color::Rgb(0xef, 0xf1, 0xf5),
            fg: Color::Rgb(0x4c, 0x4f, 0x69),
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
