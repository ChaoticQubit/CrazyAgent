use alacritty_terminal::vte::ansi::Rgb;
use std::path::Path;

/// The longest font list a style may carry, in bytes.
pub const FONT_LIST_BYTES_MAX: usize = 256;

/// The font size bounds, in CSS pixels.
pub const FONT_PX_MIN: f64 = 6.0;
pub const FONT_PX_MAX: f64 = 72.0;

/// The longest colour string accepted, in bytes.
const COLOR_BYTES_MAX: usize = 64;

/// The monospace families the native view can draw with, by the names a CSS font list uses,
/// and the system file of each, in the order a list is matched against. The system SF Mono
/// file is left out: its default instance is Light (weight class 295), not the Regular that
/// `ui-monospace` gives in the webview, so `ui-monospace` draws with Menlo.
const FAMILIES: [(&str, &str, &str); 3] = [
    ("menlo", "Menlo", "/System/Library/Fonts/Menlo.ttc"),
    ("ui-monospace", "Menlo", "/System/Library/Fonts/Menlo.ttc"),
    ("monospace", "Menlo", "/System/Library/Fonts/Menlo.ttc"),
];

/// The colours a native view draws with, as the panel's `--terminal-*` tokens give them:
/// the selection is an overlay of `selection` at `selection_alpha` over the cell background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub cursor_accent: Rgb,
    pub selection: Rgb,
    pub selection_alpha: f32,
    pub ansi: [Rgb; 16],
}

const fn hex(value: u32) -> Rgb {
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

impl Default for Theme {
    fn default() -> Theme {
        let ansi = [
            0x2a2723, 0xf2867e, 0x6cc08e, 0xe3b45e, 0x8fb3d9, 0xd4a0c8, 0x7fc4bd, 0xd8d2c6,
            0x8a8477, 0xff9d94, 0x8fd6aa, 0xf0c77a, 0xa9c6e6, 0xe2b8d8, 0x9dd6cf, 0xf6f2ea,
        ];
        Theme {
            background: hex(0x161513),
            foreground: hex(0xeeeae2),
            cursor: hex(0xff6a45),
            cursor_accent: hex(0x161513),
            selection: hex(0xff6a45),
            selection_alpha: 0.32,
            ansi: ansi.map(hex),
        }
    }
}

/// The theme as the page sends it: one CSS colour string per field, named as xterm names
/// them.
#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeText {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub cursor_accent: String,
    pub selection_background: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

fn channel(text: &str) -> Option<u8> {
    let value: f64 = text.trim().parse().ok()?;
    (0.0..=255.0).contains(&value).then(|| value.round() as u8)
}

/// Parses a CSS colour the theme tokens use: `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()` or
/// `rgba()`. Input: the text. Output: the colour and its alpha (0 to 1), or none.
pub fn css_color(text: &str) -> Option<(Rgb, f32)> {
    let text = text.trim();
    if text.len() > COLOR_BYTES_MAX || !text.is_ascii() {
        return None;
    }
    if let Some(digits) = text.strip_prefix('#') {
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let wide = |n: usize| u8::from_str_radix(digits.get(n..n + 2)?, 16).ok();
        let short = |n: usize| Some(u8::from_str_radix(digits.get(n..=n)?, 16).ok()? * 17);
        let (r, g, b, a) = match digits.len() {
            3 => (short(0)?, short(1)?, short(2)?, 255),
            6 => (wide(0)?, wide(2)?, wide(4)?, 255),
            8 => (wide(0)?, wide(2)?, wide(4)?, wide(6)?),
            _ => return None,
        };
        return Some((Rgb { r, g, b }, f32::from(a) / 255.0));
    }
    let inner = text
        .strip_prefix("rgba(")
        .or_else(|| text.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').collect();
    let alpha = match parts.len() {
        3 => 1.0,
        4 => parts[3]
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|a| (0.0..=1.0).contains(a))?,
        _ => return None,
    };
    let color = Rgb {
        r: channel(parts[0])?,
        g: channel(parts[1])?,
        b: channel(parts[2])?,
    };
    assert!((0.0..=1.0).contains(&alpha));
    Some((color, alpha))
}

/// One colour drawn at an alpha over another. Inputs: the top colour, its alpha and the
/// colour under it. Output: the colour shown.
pub fn blend(top: Rgb, alpha: f32, under: Rgb) -> Rgb {
    assert!((0.0..=1.0).contains(&alpha));
    let mix = |a: u8, b: u8| (f32::from(a) * alpha + f32::from(b) * (1.0 - alpha)).round() as u8;
    let shown = Rgb {
        r: mix(top.r, under.r),
        g: mix(top.g, under.g),
        b: mix(top.b, under.b),
    };
    assert!(alpha < 1.0 || shown == top);
    shown
}

impl Theme {
    /// The theme from the page's colour strings; every colour but the selection must be
    /// opaque. Input: the strings. Output: the theme, or `native_terminal_style_invalid`.
    pub fn from_text(text: &ThemeText) -> Result<Theme, &'static str> {
        let solid = |field: &str| match css_color(field) {
            Some((color, alpha)) if alpha >= 1.0 => Ok(color),
            _ => Err("native_terminal_style_invalid"),
        };
        let ansi = [
            &text.black,
            &text.red,
            &text.green,
            &text.yellow,
            &text.blue,
            &text.magenta,
            &text.cyan,
            &text.white,
            &text.bright_black,
            &text.bright_red,
            &text.bright_green,
            &text.bright_yellow,
            &text.bright_blue,
            &text.bright_magenta,
            &text.bright_cyan,
            &text.bright_white,
        ];
        let mut colors = [Rgb::default(); 16];
        for (slot, field) in colors.iter_mut().zip(ansi) {
            *slot = solid(field)?;
        }
        let (selection, selection_alpha) =
            css_color(&text.selection_background).ok_or("native_terminal_style_invalid")?;
        let theme = Theme {
            background: solid(&text.background)?,
            foreground: solid(&text.foreground)?,
            cursor: solid(&text.cursor)?,
            cursor_accent: solid(&text.cursor_accent)?,
            selection,
            selection_alpha,
            ansi: colors,
        };
        assert!((0.0..=1.0).contains(&theme.selection_alpha));
        Ok(theme)
    }
}

/// How every native view draws, as the panel's style command gives it: the font list, its
/// size in CSS pixels, whether the cursor blinks and the theme.
#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StyleText {
    pub font_family: String,
    pub font_size: f64,
    pub cursor_blink: bool,
    pub theme: ThemeText,
}

/// A checked style: the font family drawn with and its file, the size in CSS pixels, the
/// cursor blink and the colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub family: &'static str,
    pub file: &'static Path,
    pub font_px: f64,
    pub blink: bool,
    pub theme: Theme,
}

impl Style {
    /// The style from the page's text. Input: the text. Output: the style, or
    /// `native_terminal_style_invalid` for a size out of bounds, no usable font or a bad
    /// colour.
    pub fn from_text(text: &StyleText) -> Result<Style, &'static str> {
        let size = text.font_size;
        if !(FONT_PX_MIN..=FONT_PX_MAX).contains(&size) {
            return Err("native_terminal_style_invalid");
        }
        let (family, file) = font_for(&text.font_family)?;
        let style = Style {
            family,
            file,
            font_px: size,
            blink: text.cursor_blink,
            theme: Theme::from_text(&text.theme)?,
        };
        assert!(style.font_px.is_finite());
        Ok(style)
    }
}

/// A font's metrics in em: the advance of one cell and the ascent and descent of a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceEm {
    pub advance: f64,
    pub ascent: f64,
    pub descent: f64,
}

/// The cell size in device pixels, by the rule today's xterm panel uses (its WebGL renderer
/// floors the character width and ceils the line height at the device scale, letter
/// spacing 0, line height 1), so a host rect gives the same grid. Inputs: the font metrics,
/// the size in CSS pixels and the backing scale. Output: (width, height) in device pixels.
pub fn cell_px(face: FaceEm, font_px: f64, scale: f64) -> (f64, f64) {
    assert!((FONT_PX_MIN..=FONT_PX_MAX).contains(&font_px));
    assert!((1.0..=4.0).contains(&scale));
    assert!(face.advance > 0.0 && face.ascent + face.descent > 0.0);
    let width = (face.advance * font_px * scale).floor();
    let height = ((face.ascent + face.descent) * font_px * scale).ceil();
    assert!(width >= 1.0 && height >= 1.0);
    (width, height)
}

/// The first family of a CSS font list the native view can draw with and whose file is
/// installed: its family name and file. Families it does not know count as not installed.
/// Input: the list. Output: (family, file), or `native_terminal_style_invalid`.
pub fn font_for(list: &str) -> Result<(&'static str, &'static Path), &'static str> {
    if list.len() > FONT_LIST_BYTES_MAX {
        return Err("native_terminal_style_invalid");
    }
    let found = list
        .split(',')
        .map(|name| {
            name.trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_ascii_lowercase()
        })
        .find_map(|name| {
            FAMILIES
                .iter()
                .find(|(css, _, file)| *css == name && Path::new(file).is_file())
                .map(|(_, family, file)| (*family, Path::new(*file)))
        });
    let (family, file) = found.ok_or("native_terminal_style_invalid")?;
    assert!(file.is_absolute());
    assert!(!family.is_empty());
    Ok((family, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_colors_parse_the_token_forms() {
        let rgb = |r, g, b| Rgb { r, g, b };
        assert_eq!(css_color("#161513"), Some((rgb(0x16, 0x15, 0x13), 1.0)));
        assert_eq!(css_color("#fff"), Some((rgb(255, 255, 255), 1.0)));
        assert_eq!(css_color("#ff000080").map(|c| c.0), Some(rgb(255, 0, 0)));
        assert_eq!(
            css_color("rgba(255, 106, 69, 0.32)"),
            Some((rgb(255, 106, 69), 0.32))
        );
        assert_eq!(css_color(" rgb(1,2,3) "), Some((rgb(1, 2, 3), 1.0)));
        assert_eq!(css_color("rgba(1, 2, 3, 2)"), None);
        assert_eq!(css_color("rgb(256, 0, 0)"), None);
        assert_eq!(css_color("#12345"), None);
        assert_eq!(css_color("red"), None);
        assert_eq!(css_color("#+f+f+f"), None);
        assert_eq!(css_color("#+1+2+3+4"), None);
        assert_eq!(css_color(&"#".repeat(COLOR_BYTES_MAX + 1)), None);
    }

    #[test]
    fn the_selection_blends_over_the_cell_background() {
        let under = Rgb {
            r: 0x16,
            g: 0x15,
            b: 0x13,
        };
        let top = Rgb {
            r: 255,
            g: 106,
            b: 69,
        };
        assert_eq!(
            blend(top, 0.32, under),
            Rgb {
                r: 97,
                g: 48,
                b: 35
            }
        );
        assert_eq!(blend(top, 1.0, under), top);
        assert_eq!(blend(top, 0.0, under), under);
    }

    #[test]
    fn the_page_theme_matches_the_defaults_and_refuses_bad_colours() {
        let json = r##"{"background":"#161513","foreground":"#eeeae2","cursor":"#ff6a45",
            "cursorAccent":"#161513","selectionBackground":"rgba(255, 106, 69, 0.32)",
            "black":"#2a2723","red":"#f2867e","green":"#6cc08e","yellow":"#e3b45e",
            "blue":"#8fb3d9","magenta":"#d4a0c8","cyan":"#7fc4bd","white":"#d8d2c6",
            "brightBlack":"#8a8477","brightRed":"#ff9d94","brightGreen":"#8fd6aa",
            "brightYellow":"#f0c77a","brightBlue":"#a9c6e6","brightMagenta":"#e2b8d8",
            "brightCyan":"#9dd6cf","brightWhite":"#f6f2ea"}"##;
        let mut text: ThemeText = serde_json::from_str(json).expect("page theme");
        assert_eq!(Theme::from_text(&text), Ok(Theme::default()));
        text.cursor = "rgba(1, 2, 3, 0.5)".into();
        assert_eq!(
            Theme::from_text(&text),
            Err("native_terminal_style_invalid")
        );
        let extra = json.replace("\"brightWhite\"", "\"other\":\"#000\",\"brightWhite\"");
        assert!(serde_json::from_str::<ThemeText>(&extra).is_err());
    }

    #[test]
    fn cells_follow_the_xterm_rounding() {
        let jet = FaceEm {
            advance: 0.6,
            ascent: 1.02,
            descent: 0.3,
        };
        assert_eq!(cell_px(jet, 13.0, 2.0), (15.0, 35.0));
        assert_eq!(cell_px(jet, 13.0, 1.0), (7.0, 18.0));
        let menlo = FaceEm {
            advance: 1233.0 / 2048.0,
            ascent: 1901.0 / 2048.0,
            descent: 0.2358,
        };
        assert_eq!(cell_px(menlo, 13.0, 2.0), (15.0, 31.0));
    }

    #[test]
    fn a_style_checks_its_size_and_font() {
        let theme = Theme::default();
        let text = |size: f64, family: &str| StyleText {
            font_family: family.into(),
            font_size: size,
            cursor_blink: true,
            theme: ThemeText {
                background: "#161513".into(),
                foreground: "#eeeae2".into(),
                cursor: "#ff6a45".into(),
                cursor_accent: "#161513".into(),
                selection_background: "rgba(255, 106, 69, 0.32)".into(),
                black: "#2a2723".into(),
                red: "#f2867e".into(),
                green: "#6cc08e".into(),
                yellow: "#e3b45e".into(),
                blue: "#8fb3d9".into(),
                magenta: "#d4a0c8".into(),
                cyan: "#7fc4bd".into(),
                white: "#d8d2c6".into(),
                bright_black: "#8a8477".into(),
                bright_red: "#ff9d94".into(),
                bright_green: "#8fd6aa".into(),
                bright_yellow: "#f0c77a".into(),
                bright_blue: "#a9c6e6".into(),
                bright_magenta: "#e2b8d8".into(),
                bright_cyan: "#9dd6cf".into(),
                bright_white: "#f6f2ea".into(),
            },
        };
        let style = Style::from_text(&text(13.0, "Menlo")).expect("menlo style");
        assert_eq!(
            (style.family, style.font_px, style.blink),
            ("Menlo", 13.0, true)
        );
        assert_eq!(style.theme, theme);
        for size in [5.9, 72.1, f64::NAN, f64::INFINITY] {
            assert_eq!(
                Style::from_text(&text(size, "Menlo")),
                Err("native_terminal_style_invalid")
            );
        }
        assert_eq!(
            Style::from_text(&text(13.0, "Courier")),
            Err("native_terminal_style_invalid")
        );
    }

    #[test]
    fn the_first_known_installed_family_is_used() {
        let list = "\"JetBrains Mono\", ui-monospace, \"SF Mono\", Menlo, monospace";
        assert_eq!(font_for(list).map(|f| f.0), Ok("Menlo"));
        assert_eq!(font_for("\"SF Mono\""), Err("native_terminal_style_invalid"));
        assert_eq!(font_for("Courier, Menlo").map(|f| f.0), Ok("Menlo"));
        assert_eq!(font_for("Courier"), Err("native_terminal_style_invalid"));
        let long = "x,".repeat(FONT_LIST_BYTES_MAX);
        assert_eq!(font_for(&long), Err("native_terminal_style_invalid"));
    }
}
