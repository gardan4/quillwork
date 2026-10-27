//! The document theme (`word/theme/theme1.xml`): colour scheme and font scheme.
//!
//! WordprocessingML refers to theme colours (`w:color/@w:themeColor`, `w:shd/@w:themeFill`)
//! and theme fonts (`w:rFonts/@w:asciiTheme`, …) by name; this module turns those names into
//! concrete RGB values and typeface names. See `docs/DESIGN.md` §4.3.

use std::fmt;

use crate::xml::{Document, NodeId, ns};
use crate::{Error, Result};

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parse `RRGGBB` (case-insensitive, an optional leading `#`).
    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.trim();
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// `RRGGBB`, upper case, no `#` (the form WordprocessingML uses).
    pub fn hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    /// Apply a WordprocessingML `themeTint` (0–255): lightens towards white in HSL space.
    pub fn tint(self, tint: u8) -> Rgb {
        let t = f64::from(tint) / 255.0;
        let (h, s, l) = self.to_hsl();
        Rgb::from_hsl(h, s, l * t + (1.0 - t))
    }

    /// Apply a WordprocessingML `themeShade` (0–255): darkens towards black in HSL space.
    pub fn shade(self, shade: u8) -> Rgb {
        let f = f64::from(shade) / 255.0;
        let (h, s, l) = self.to_hsl();
        Rgb::from_hsl(h, s, l * f)
    }

    fn to_hsl(self) -> (f64, f64, f64) {
        let r = f64::from(self.0) / 255.0;
        let g = f64::from(self.1) / 255.0;
        let b = f64::from(self.2) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let c = max - min;
        if c <= f64::EPSILON {
            return (0.0, 0.0, l);
        }
        let s = c / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            ((g - b) / c).rem_euclid(6.0)
        } else if max == g {
            (b - r) / c + 2.0
        } else {
            (r - g) / c + 4.0
        } * 60.0;
        (h, s, l)
    }

    fn from_hsl(h: f64, s: f64, l: f64) -> Rgb {
        let l = l.clamp(0.0, 1.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s.clamp(0.0, 1.0);
        let hp = h / 60.0;
        let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match hp as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = l - c / 2.0;
        let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Rgb(to(r), to(g), to(b))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

/// The twelve colour slots of a DrawingML colour scheme (`a:clrScheme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemeColor {
    Dark1,
    Light1,
    Dark2,
    Light2,
    Accent1,
    Accent2,
    Accent3,
    Accent4,
    Accent5,
    Accent6,
    Hyperlink,
    FollowedHyperlink,
}

impl SchemeColor {
    const ALL: [(SchemeColor, &'static str, &'static str); 12] = [
        (SchemeColor::Dark1, "dk1", "dark1"),
        (SchemeColor::Light1, "lt1", "light1"),
        (SchemeColor::Dark2, "dk2", "dark2"),
        (SchemeColor::Light2, "lt2", "light2"),
        (SchemeColor::Accent1, "accent1", "accent1"),
        (SchemeColor::Accent2, "accent2", "accent2"),
        (SchemeColor::Accent3, "accent3", "accent3"),
        (SchemeColor::Accent4, "accent4", "accent4"),
        (SchemeColor::Accent5, "accent5", "accent5"),
        (SchemeColor::Accent6, "accent6", "accent6"),
        (SchemeColor::Hyperlink, "hlink", "hyperlink"),
        (SchemeColor::FollowedHyperlink, "folHlink", "followedHyperlink"),
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// From the DrawingML element name (`dk1`, `accent3`, `hlink`, …).
    pub fn from_drawingml(s: &str) -> Option<SchemeColor> {
        Self::ALL.iter().find(|(_, d, _)| *d == s).map(|(c, _, _)| *c)
    }

    /// From the WordprocessingML `ST_ThemeColor` / `clrSchemeMapping` value (`dark1`, `accent3`, …).
    pub fn from_wml(s: &str) -> Option<SchemeColor> {
        Self::ALL.iter().find(|(_, _, w)| *w == s).map(|(c, _, _)| *c)
    }
}

/// `ST_ThemeColor`: a theme colour as WordprocessingML names it. The four mapped names
/// (`background1`, `text1`, …) go through the settings' `w:clrSchemeMapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeColor {
    Scheme(SchemeColor),
    Background1,
    Text1,
    Background2,
    Text2,
}

impl ThemeColor {
    /// Parse an `ST_ThemeColor` value. `none` and unknown values give `None`.
    pub fn parse(s: &str) -> Option<ThemeColor> {
        Some(match s {
            "background1" => ThemeColor::Background1,
            "text1" => ThemeColor::Text1,
            "background2" => ThemeColor::Background2,
            "text2" => ThemeColor::Text2,
            other => ThemeColor::Scheme(SchemeColor::from_wml(other)?),
        })
    }
}

/// `w:clrSchemeMapping` from `settings.xml`: which scheme slot each mapped name uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorMapping {
    pub background1: SchemeColor,
    pub text1: SchemeColor,
    pub background2: SchemeColor,
    pub text2: SchemeColor,
}

impl Default for ColorMapping {
    fn default() -> Self {
        ColorMapping {
            background1: SchemeColor::Light1,
            text1: SchemeColor::Dark1,
            background2: SchemeColor::Light2,
            text2: SchemeColor::Dark2,
        }
    }
}

impl ColorMapping {
    /// The scheme slot a theme colour refers to.
    pub fn map(&self, c: ThemeColor) -> SchemeColor {
        match c {
            ThemeColor::Scheme(s) => s,
            ThemeColor::Background1 => self.background1,
            ThemeColor::Text1 => self.text1,
            ThemeColor::Background2 => self.background2,
            ThemeColor::Text2 => self.text2,
        }
    }
}

/// `ST_Theme`: a theme font reference (`w:rFonts/@w:asciiTheme` and friends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeFont {
    MajorAscii,
    MajorHAnsi,
    MajorEastAsia,
    MajorBidi,
    MinorAscii,
    MinorHAnsi,
    MinorEastAsia,
    MinorBidi,
}

impl ThemeFont {
    pub fn parse(s: &str) -> Option<ThemeFont> {
        Some(match s {
            "majorAscii" => ThemeFont::MajorAscii,
            "majorHAnsi" => ThemeFont::MajorHAnsi,
            "majorEastAsia" => ThemeFont::MajorEastAsia,
            "majorBidi" => ThemeFont::MajorBidi,
            "minorAscii" => ThemeFont::MinorAscii,
            "minorHAnsi" => ThemeFont::MinorHAnsi,
            "minorEastAsia" => ThemeFont::MinorEastAsia,
            "minorBidi" => ThemeFont::MinorBidi,
            _ => return None,
        })
    }

    fn is_major(self) -> bool {
        matches!(self, ThemeFont::MajorAscii | ThemeFont::MajorHAnsi | ThemeFont::MajorEastAsia | ThemeFont::MajorBidi)
    }
}

/// One `a:majorFont` / `a:minorFont`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontCollection {
    /// `a:latin/@typeface`.
    pub latin: Option<String>,
    /// `a:ea/@typeface` (often empty: then the per-script `a:font` for the East Asian language is used).
    pub east_asia: Option<String>,
    /// `a:cs/@typeface` (often empty: then the per-script `a:font` for the bidi language is used).
    pub complex: Option<String>,
    /// `a:font` entries as (script tag, typeface), e.g. ("Jpan", "游明朝").
    pub scripts: Vec<(String, String)>,
}

impl FontCollection {
    fn script(&self, tag: &str) -> Option<&str> {
        self.scripts.iter().find(|(s, _)| s == tag).map(|(_, f)| f.as_str())
    }
}

/// A parsed theme part.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Theme {
    colors: [Option<Rgb>; 12],
    pub major: FontCollection,
    pub minor: FontCollection,
}

impl Theme {
    /// Parse `word/theme/theme1.xml`. Missing pieces are left empty; only a part that is not a
    /// DrawingML theme at all is an error.
    pub fn parse(doc: &Document) -> Result<Theme> {
        let root = doc.root();
        if !doc.is(root, ns::A, "theme") && !doc.is(root, ns::A, "themeOverride") {
            return Err(Error::Style(format!(
                "theme part root is <{}>, expected <a:theme>",
                doc.name(root).unwrap_or("?")
            )));
        }
        let mut theme = Theme::default();
        let elements = doc.find_child(root, ns::A, "themeElements").unwrap_or(root);
        if let Some(scheme) = doc.find_child(elements, ns::A, "clrScheme") {
            for c in doc.child_elements(scheme) {
                if doc.namespace(c) != Some(ns::A) {
                    continue;
                }
                let Some(slot) = doc.local_name(c).and_then(SchemeColor::from_drawingml) else { continue };
                theme.colors[slot.index()] = scheme_color_value(doc, c);
            }
        }
        if let Some(fonts) = doc.find_child(elements, ns::A, "fontScheme") {
            if let Some(m) = doc.find_child(fonts, ns::A, "majorFont") {
                theme.major = font_collection(doc, m);
            }
            if let Some(m) = doc.find_child(fonts, ns::A, "minorFont") {
                theme.minor = font_collection(doc, m);
            }
        }
        Ok(theme)
    }

    /// RGB of a scheme slot.
    pub fn scheme_color(&self, c: SchemeColor) -> Option<Rgb> {
        self.colors[c.index()]
    }

    /// RGB of a WordprocessingML theme colour through a colour mapping.
    pub fn color(&self, c: ThemeColor, mapping: &ColorMapping) -> Option<Rgb> {
        self.scheme_color(mapping.map(c))
    }

    /// Typeface for a theme font reference. For the East Asian and bidi slots an empty
    /// `a:ea` / `a:cs` falls back to the `a:font` entry for `lang` (a BCP-47 tag such as
    /// `ja-JP` or `ar-SA`).
    pub fn font(&self, f: ThemeFont, lang: Option<&str>) -> Option<&str> {
        let coll = if f.is_major() { &self.major } else { &self.minor };
        let (main, by_lang) = match f {
            ThemeFont::MajorAscii | ThemeFont::MajorHAnsi | ThemeFont::MinorAscii | ThemeFont::MinorHAnsi => {
                (coll.latin.as_deref(), false)
            }
            ThemeFont::MajorEastAsia | ThemeFont::MinorEastAsia => (coll.east_asia.as_deref(), true),
            ThemeFont::MajorBidi | ThemeFont::MinorBidi => (coll.complex.as_deref(), true),
        };
        match main {
            Some(name) if !name.is_empty() => Some(name),
            _ if by_lang => lang.and_then(script_for_lang).and_then(|tag| coll.script(tag)),
            _ => None,
        }
    }
}

fn scheme_color_value(doc: &Document, slot: NodeId) -> Option<Rgb> {
    for c in doc.child_elements(slot) {
        if doc.is(c, ns::A, "srgbClr") {
            return doc.attr(c, "val").and_then(|v| Rgb::from_hex(&v));
        }
        if doc.is(c, ns::A, "sysClr") {
            if let Some(v) = doc.attr(c, "lastClr").and_then(|v| Rgb::from_hex(&v)) {
                return Some(v);
            }
            return match doc.attr(c, "val").as_deref() {
                Some("windowText") => Some(Rgb(0, 0, 0)),
                Some("window") => Some(Rgb(255, 255, 255)),
                _ => None,
            };
        }
    }
    None
}

fn font_collection(doc: &Document, n: NodeId) -> FontCollection {
    let mut fc = FontCollection::default();
    for c in doc.child_elements(n) {
        if doc.namespace(c) != Some(ns::A) {
            continue;
        }
        let face = doc.attr(c, "typeface").map(|v| v.into_owned());
        match doc.local_name(c) {
            Some("latin") => fc.latin = face,
            Some("ea") => fc.east_asia = face,
            Some("cs") => fc.complex = face,
            Some("font") => {
                if let (Some(script), Some(face)) = (doc.attr(c, "script"), face) {
                    fc.scripts.push((script.into_owned(), face));
                }
            }
            _ => {}
        }
    }
    fc
}

/// The DrawingML script tag (`a:font/@script`) used for a language's theme font.
pub fn script_for_lang(lang: &str) -> Option<&'static str> {
    let lower = lang.to_ascii_lowercase();
    let (primary, region) = match lower.split_once(['-', '_']) {
        Some((p, r)) => (p, r),
        None => (lower.as_str(), ""),
    };
    Some(match primary {
        "ja" => "Jpan",
        "ko" => "Hang",
        "zh" => match region {
            "tw" | "hk" | "mo" | "hant" => "Hant",
            _ => "Hans",
        },
        "ar" | "fa" | "ur" | "ps" | "sd" | "ug" => "Arab",
        "he" | "yi" => "Hebr",
        "th" => "Thai",
        "hi" | "mr" | "ne" | "sa" => "Deva",
        "bn" | "as" => "Beng",
        "ta" => "Taml",
        "te" => "Telu",
        "kn" => "Knda",
        "ml" => "Mlym",
        "gu" => "Gujr",
        "pa" => "Guru",
        "or" => "Orya",
        "si" => "Sinh",
        "km" => "Khmr",
        "lo" => "Laoo",
        "my" => "Mymr",
        "bo" => "Tibt",
        "am" | "ti" => "Ethi",
        "ka" => "Geor",
        "hy" => "Armn",
        "mn" => "Mong",
        "syr" => "Syrc",
        "dv" => "Thaa",
        "iu" => "Cans",
        "chr" => "Cher",
        "ii" => "Yiii",
        _ => return None,
    })
}
