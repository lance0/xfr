//! Color theme definitions for the UI.
//!
//! Provides 11 built-in themes: default, kawaii, cyber, dracula, monochrome,
//! matrix, nord, gruvbox, catppuccin, tokyo_night, solarized.
//! Themes can be selected via the `--theme` CLI flag.

use ratatui::style::Color;
use std::borrow::Cow;

/// Palette-independent mid grey (xterm 244, #808080) for secondary text.
const DIM_GREY: Color = Color::Indexed(244);

/// All themeable colors in the application
#[derive(Clone, Debug, Hash)]
pub struct Theme {
    name: Cow<'static, str>,

    // UI chrome
    pub border: Color,
    pub border_focused: Color,
    pub text: Color,
    pub text_dim: Color,
    pub highlight_bg: Color,

    // Status indicators
    pub success: Color,
    pub warning: Color,
    pub error: Color,

    // Accents
    pub accent: Color,
    pub header: Color,

    // Graph colors
    pub graph_primary: Color,
    pub graph_secondary: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_theme()
    }
}

impl Theme {
    /// The default theme
    pub fn default_theme() -> Self {
        Self {
            name: Cow::Borrowed("default"),

            border: Color::Cyan,
            border_focused: Color::Cyan,
            // Follow the terminal foreground on both light and dark backgrounds.
            text: Color::Reset,
            // No named ANSI grey is safe for secondary text: Solarized Dark
            // maps ANSI 8 (DarkGray) to its background and light palettes put
            // ANSI 7 (Gray) next to theirs. A fixed mid grey from the 256-colour
            // ramp (#808080) stays legible on dark and light backgrounds alike.
            text_dim: DIM_GREY,
            highlight_bg: Color::DarkGray,

            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,

            accent: Color::Yellow,
            header: Color::Cyan,

            graph_primary: Color::Green,
            graph_secondary: Color::Cyan,
        }
    }

    /// Kawaii theme - cute pastel colors
    pub fn kawaii() -> Self {
        Self {
            name: Cow::Borrowed("kawaii"),

            border: Color::Rgb(255, 182, 214),
            border_focused: Color::Rgb(255, 182, 214),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(180, 180, 200),
            highlight_bg: Color::Rgb(60, 50, 70),

            success: Color::Rgb(152, 255, 200),
            warning: Color::Rgb(255, 200, 152),
            error: Color::Rgb(255, 121, 162),

            accent: Color::Rgb(214, 182, 255),
            header: Color::Rgb(255, 182, 214),

            graph_primary: Color::Rgb(152, 255, 200),
            graph_secondary: Color::Rgb(255, 182, 214),
        }
    }

    /// Cyber/Futuristic theme - neon on dark
    pub fn cyber() -> Self {
        Self {
            name: Cow::Borrowed("cyber"),

            border: Color::Rgb(0, 255, 255),
            border_focused: Color::Rgb(0, 255, 255),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(121, 121, 138),
            highlight_bg: Color::Rgb(20, 20, 35),

            success: Color::Rgb(0, 255, 150),
            warning: Color::Rgb(255, 200, 0),
            error: Color::Rgb(255, 50, 100),

            accent: Color::Rgb(255, 0, 255),
            header: Color::Rgb(0, 255, 255),

            graph_primary: Color::Rgb(0, 255, 150),
            graph_secondary: Color::Rgb(0, 255, 255),
        }
    }

    /// Dracula theme
    pub fn dracula() -> Self {
        Self {
            name: Cow::Borrowed("dracula"),

            border: Color::Rgb(189, 147, 249),
            border_focused: Color::Rgb(189, 147, 249),
            text: Color::Rgb(248, 248, 242),
            text_dim: Color::Rgb(133, 146, 184),
            highlight_bg: Color::Rgb(68, 71, 90),

            success: Color::Rgb(80, 250, 123),
            warning: Color::Rgb(255, 184, 108),
            error: Color::Rgb(255, 85, 85),

            accent: Color::Rgb(241, 250, 140),
            header: Color::Rgb(255, 121, 198),

            graph_primary: Color::Rgb(80, 250, 123),
            graph_secondary: Color::Rgb(189, 147, 249),
        }
    }

    /// Monochrome theme — high-contrast grayscale for glare, monochrome
    /// displays, and `NO_COLOR` runs (issue #158).
    ///
    /// Built from terminal-adaptive ANSI colors, never `Rgb`: named colors
    /// are remapped by the terminal's own palette, so the theme stays
    /// readable on both dark and light backgrounds. (A hardcoded white is
    /// invisible on a light terminal — and a light background is exactly
    /// what wins in direct sun, where dark screens act as mirrors.)
    /// Deliberately few levels: glare washes out mid-tone grays first, so
    /// everything collapses to Reset / Gray / one fixed mid grey, with
    /// maximum contrast (the terminal's default foreground) on anything that
    /// matters. Borders and secondary text use the fixed grey rather than
    /// DarkGray because Solarized Dark draws ANSI 8 in its background colour,
    /// which erased every border and label. Loss severity stays legible
    /// without hue via the modifier encoding in the sparkline renderer.
    pub fn monochrome() -> Self {
        Self {
            name: Cow::Borrowed("monochrome"),

            border: DIM_GREY,
            border_focused: Color::Gray,
            text: Color::Reset,
            text_dim: DIM_GREY,
            highlight_bg: Color::DarkGray,

            success: Color::Reset,
            // A real color, not Reset: also used as a *background* (update
            // banner renders black-on-warning).
            warning: Color::Gray,
            error: Color::Reset,

            accent: Color::Reset,
            header: Color::Reset,

            graph_primary: Color::Reset,
            graph_secondary: Color::Gray,
        }
    }

    /// Matrix theme - green on black hacker style
    pub fn matrix() -> Self {
        Self {
            name: Cow::Borrowed("matrix"),

            border: Color::Rgb(0, 255, 0),
            border_focused: Color::Rgb(0, 255, 0),
            text: Color::Rgb(0, 255, 0),
            text_dim: Color::Rgb(0, 170, 60),
            highlight_bg: Color::Rgb(0, 20, 0),

            success: Color::Rgb(0, 255, 0),
            warning: Color::Rgb(200, 255, 100),
            error: Color::Rgb(255, 100, 100),

            accent: Color::Rgb(100, 255, 100),
            header: Color::Rgb(0, 255, 0),

            graph_primary: Color::Rgb(0, 255, 0),
            graph_secondary: Color::Rgb(0, 200, 0),
        }
    }

    /// Nord theme - arctic, north-bluish colors
    pub fn nord() -> Self {
        Self {
            name: Cow::Borrowed("nord"),

            border: Color::Rgb(136, 192, 208),
            border_focused: Color::Rgb(136, 192, 208),
            text: Color::Rgb(236, 239, 244),
            text_dim: Color::Rgb(147, 158, 178),
            highlight_bg: Color::Rgb(59, 66, 82),

            success: Color::Rgb(163, 190, 140),
            warning: Color::Rgb(235, 203, 139),
            error: Color::Rgb(191, 97, 106),

            accent: Color::Rgb(235, 203, 139),
            header: Color::Rgb(136, 192, 208),

            graph_primary: Color::Rgb(163, 190, 140),
            graph_secondary: Color::Rgb(136, 192, 208),
        }
    }

    /// Gruvbox theme - retro groove colors
    pub fn gruvbox() -> Self {
        Self {
            name: Cow::Borrowed("gruvbox"),

            border: Color::Rgb(254, 128, 25),
            border_focused: Color::Rgb(254, 128, 25),
            text: Color::Rgb(235, 219, 178),
            text_dim: Color::Rgb(168, 153, 132),
            highlight_bg: Color::Rgb(80, 73, 69),

            success: Color::Rgb(184, 187, 38),
            warning: Color::Rgb(250, 189, 47),
            error: Color::Rgb(251, 73, 52),

            accent: Color::Rgb(250, 189, 47),
            header: Color::Rgb(254, 128, 25),

            graph_primary: Color::Rgb(184, 187, 38),
            graph_secondary: Color::Rgb(254, 128, 25),
        }
    }

    /// Catppuccin Mocha theme - soothing pastel colors
    pub fn catppuccin() -> Self {
        Self {
            name: Cow::Borrowed("catppuccin"),

            border: Color::Rgb(203, 166, 247),
            border_focused: Color::Rgb(203, 166, 247),
            text: Color::Rgb(205, 214, 244),
            text_dim: Color::Rgb(147, 153, 178),
            highlight_bg: Color::Rgb(88, 91, 112),

            success: Color::Rgb(166, 227, 161),
            warning: Color::Rgb(249, 226, 175),
            error: Color::Rgb(243, 139, 168),

            accent: Color::Rgb(249, 226, 175),
            header: Color::Rgb(245, 194, 231),

            graph_primary: Color::Rgb(166, 227, 161),
            graph_secondary: Color::Rgb(203, 166, 247),
        }
    }

    /// Tokyo Night theme
    pub fn tokyo_night() -> Self {
        Self {
            name: Cow::Borrowed("tokyo_night"),

            border: Color::Rgb(187, 154, 247),
            border_focused: Color::Rgb(187, 154, 247),
            text: Color::Rgb(192, 202, 245),
            text_dim: Color::Rgb(125, 132, 164),
            highlight_bg: Color::Rgb(59, 66, 97),

            success: Color::Rgb(158, 206, 106),
            warning: Color::Rgb(224, 175, 104),
            error: Color::Rgb(247, 118, 142),

            accent: Color::Rgb(224, 175, 104),
            header: Color::Rgb(187, 154, 247),

            graph_primary: Color::Rgb(158, 206, 106),
            graph_secondary: Color::Rgb(187, 154, 247),
        }
    }

    /// Solarized Dark theme
    pub fn solarized() -> Self {
        Self {
            name: Cow::Borrowed("solarized"),

            border: Color::Rgb(42, 161, 152),
            border_focused: Color::Rgb(42, 161, 152),
            text: Color::Rgb(147, 161, 161),
            text_dim: Color::Rgb(131, 148, 150),
            highlight_bg: Color::Rgb(7, 54, 66),

            success: Color::Rgb(133, 153, 0),
            warning: Color::Rgb(181, 137, 0),
            error: Color::Rgb(220, 50, 47),

            accent: Color::Rgb(181, 137, 0),
            header: Color::Rgb(203, 75, 22),

            graph_primary: Color::Rgb(133, 153, 0),
            graph_secondary: Color::Rgb(42, 161, 152),
        }
    }

    /// Get a theme by name
    pub fn by_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "kawaii" => Self::kawaii(),
            "cyber" | "futuristic" => Self::cyber(),
            "monochrome" | "mono" => Self::monochrome(),
            "dracula" => Self::dracula(),
            "matrix" | "hacker" => Self::matrix(),
            "nord" => Self::nord(),
            "gruvbox" => Self::gruvbox(),
            "catppuccin" | "mocha" => Self::catppuccin(),
            "tokyo_night" | "tokyo" | "tokyonight" => Self::tokyo_night(),
            "solarized" => Self::solarized(),
            _ => Self::default_theme(),
        }
    }

    /// Get the theme name
    /// True for the theme that cannot rely on hue to carry meaning. The
    /// sparkline renderer uses this to switch heavy-loss marking from a
    /// color tint to a reverse-video pillar (issue #158/#93 follow-up).
    pub fn hue_free(&self) -> bool {
        self.name == "monochrome"
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// List all available theme names
    pub fn list() -> &'static [&'static str] {
        &[
            "default",
            "kawaii",
            "cyber",
            "dracula",
            "monochrome",
            "matrix",
            "nord",
            "gruvbox",
            "catppuccin",
            "tokyo_night",
            "solarized",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_follows_terminal_foreground() {
        let theme = Theme::default();
        assert_eq!(theme.text, Color::Reset);
        assert_eq!(theme.text_dim, Color::Indexed(244));
    }

    #[test]
    fn default_theme_uses_only_terminal_palette_colors() {
        // Named ANSI colors are remapped by the terminal for its own
        // background; fixed RGB (e.g. bright green) washes out on white.
        // `text_dim` is the one exception: no named grey survives every
        // palette (Solarized Dark draws ANSI 8 in its background colour, and
        // light palettes put ANSI 7 next to theirs), so it is a fixed mid
        // grey that keeps enough contrast against both dark and light.
        let t = Theme::default();
        assert_eq!(t.text_dim, Color::Indexed(244));
        for (role, color) in [
            ("border", t.border),
            ("border_focused", t.border_focused),
            ("text", t.text),
            ("highlight_bg", t.highlight_bg),
            ("success", t.success),
            ("warning", t.warning),
            ("error", t.error),
            ("accent", t.accent),
            ("header", t.header),
            ("graph_primary", t.graph_primary),
            ("graph_secondary", t.graph_secondary),
        ] {
            assert!(
                !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                "default {role} is fixed: {color:?}"
            );
        }
    }

    #[test]
    fn palette_themes_keep_text_and_borders_off_ansi_dark_gray() {
        // Solarized Dark draws ANSI 8 in its background colour, which erased
        // every label and border of the palette-following themes.
        for t in [Theme::default(), Theme::monochrome()] {
            for color in [t.text, t.text_dim, t.border, t.border_focused] {
                assert_ne!(color, Color::DarkGray, "{}", t.name());
            }
        }
    }

    #[test]
    fn rgb_text_is_readable_on_intended_dark_backgrounds() {
        // RGB palettes assume these dark backgrounds; ANSI palettes stay terminal controlled.
        let backgrounds = [
            ("kawaii", [40, 30, 45]),
            ("cyber", [10, 10, 20]),
            ("dracula", [40, 42, 54]),
            ("matrix", [0, 0, 0]),
            ("nord", [46, 52, 64]),
            ("gruvbox", [40, 40, 40]),
            ("catppuccin", [30, 30, 46]),
            ("tokyo_night", [26, 27, 38]),
            ("solarized", [0, 43, 54]),
        ];
        fn luminance(rgb: [u8; 3]) -> f64 {
            let linear = rgb.map(|channel| {
                let value = f64::from(channel) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            });
            linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
        }
        for (name, background) in backgrounds {
            let theme = Theme::by_name(name);
            for color in [theme.text, theme.text_dim] {
                let Color::Rgb(r, g, b) = color else {
                    panic!("{name}: expected RGB text");
                };
                let (fg, bg) = (luminance([r, g, b]), luminance(background));
                let contrast = (fg.max(bg) + 0.05) / (fg.min(bg) + 0.05);
                assert!(contrast >= 4.5, "{name}: {color:?} contrast {contrast}");
            }
        }
    }

    #[test]
    fn test_by_name_default() {
        let theme = Theme::by_name("default");
        assert_eq!(theme.name(), "default");
    }

    #[test]
    fn test_by_name_unknown_returns_default() {
        let theme = Theme::by_name("unknown_theme");
        assert_eq!(theme.name(), "default");
    }

    #[test]
    fn test_by_name_case_insensitive() {
        let lower = Theme::by_name("kawaii");
        let upper = Theme::by_name("KAWAII");
        assert_eq!(lower.name(), upper.name());
    }

    #[test]
    fn test_all_themes_loadable() {
        for name in Theme::list() {
            let theme = Theme::by_name(name);
            assert!(!theme.name().is_empty());
        }
    }
}
