//! Which `w:rFonts` slot a piece of text is shown in (ECMA-376 Part 1, §17.3.2.26).
//!
//! A simplified version of Word's rules: complex-script ranges (Arabic, Hebrew, Indic, Thai, …)
//! use the `cs` font, East Asian ranges the `eastAsia` font, ASCII the `ascii` font and
//! everything else `hAnsi`. `w:rtl` / `w:cs` on the run force the complex-script font, and
//! `w:hint="eastAsia"` sends shared characters (quotes, Latin-1 symbols, …) to the East Asian
//! font.

use serde::{Deserialize, Serialize};

/// The font slot a character is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Script {
    /// `w:ascii` (U+0000–U+007F).
    Ascii,
    /// `w:hAnsi`: other Latin, Greek, Cyrillic, symbols, …
    HAnsi,
    /// `w:eastAsia`.
    EastAsia,
    /// `w:cs`: complex scripts (RTL and Indic, Thai, …).
    ComplexScript,
}

impl Script {
    /// Uses the complex-script variants (`bCs`, `iCs`, `szCs`).
    pub fn is_complex(self) -> bool {
        self == Script::ComplexScript
    }
}

/// Is `c` neutral for picking a run's script (whitespace, digits, ASCII punctuation)?
fn is_neutral(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_digit() || c.is_ascii_punctuation() || c.is_control()
}

fn is_complex(cp: u32) -> bool {
    matches!(cp,
        0x0590..=0x08FF        // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Arabic ext.
        | 0x0900..=0x0DFF      // Indic scripts, Sinhala
        | 0x0E00..=0x0EFF      // Thai, Lao
        | 0x0F00..=0x0FFF      // Tibetan
        | 0x1000..=0x109F      // Myanmar
        | 0x1780..=0x17FF      // Khmer
        | 0x1800..=0x18AF      // Mongolian
        | 0xFB1D..=0xFDFF      // Hebrew / Arabic presentation forms A
        | 0xFE70..=0xFEFF      // Arabic presentation forms B
    )
}

fn is_east_asian(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x11FF        // Hangul Jamo
        | 0x2E80..=0x2FFF      // CJK radicals, Kangxi, ideographic description
        | 0x3000..=0x303F      // CJK symbols and punctuation
        | 0x3040..=0x31FF      // Hiragana, Katakana, Bopomofo, Hangul compat, Kanbun, …
        | 0x3200..=0x4DBF      // Enclosed CJK, compatibility, CJK ext. A
        | 0x4E00..=0x9FFF      // CJK unified ideographs
        | 0xA000..=0xA4CF      // Yi
        | 0xA960..=0xA97F      // Hangul Jamo ext. A
        | 0xAC00..=0xD7FF      // Hangul syllables, Jamo ext. B
        | 0xF900..=0xFAFF      // CJK compatibility ideographs
        | 0xFE30..=0xFE4F      // CJK compatibility forms
        | 0xFF00..=0xFFEF      // Halfwidth and fullwidth forms
        | 0x20000..=0x3FFFF    // CJK ext. B and beyond
    )
}

/// Characters whose slot `w:hint="eastAsia"` changes from `hAnsi` to `eastAsia`.
fn is_hint_shared(cp: u32) -> bool {
    matches!(cp,
        0x00A1..=0x00FF        // Latin-1 supplement symbols and letters
        | 0x02B0..=0x04FF      // modifier letters, combining marks, Greek, Cyrillic
        | 0x1E00..=0x1EFF      // Latin extended additional
        | 0x2000..=0x2BFF      // punctuation, symbols, arrows, box drawing, …
        | 0xE000..=0xF8FF      // private use
    )
}

/// The slot one character is drawn with.
pub fn script_of(c: char, east_asia_hint: bool, force_cs: bool) -> Script {
    let cp = c as u32;
    if force_cs || is_complex(cp) {
        return Script::ComplexScript;
    }
    if is_east_asian(cp) {
        return Script::EastAsia;
    }
    if cp < 0x80 {
        return Script::Ascii;
    }
    if east_asia_hint && is_hint_shared(cp) {
        return Script::EastAsia;
    }
    Script::HAnsi
}

/// The slot a whole run is shown in: that of its first non-neutral character, else the
/// first character's, else `Ascii`.
pub fn script_of_text(text: &str, east_asia_hint: bool, force_cs: bool) -> Script {
    if force_cs {
        return Script::ComplexScript;
    }
    text.chars()
        .find(|&c| !is_neutral(c))
        .or_else(|| text.chars().next())
        .map_or(Script::Ascii, |c| script_of(c, east_asia_hint, false))
}

/// Split `text` into maximal pieces drawn with one slot (neutral characters join the piece
/// before them), as (byte range, slot).
pub fn script_segments(text: &str, east_asia_hint: bool, force_cs: bool) -> Vec<(std::ops::Range<usize>, Script)> {
    let mut out: Vec<(std::ops::Range<usize>, Script)> = Vec::new();
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        let s = if is_neutral(c) && !force_cs {
            match out.last() {
                Some((_, s)) => *s,
                None => script_of_text(&text[i..], east_asia_hint, false),
            }
        } else {
            script_of(c, east_asia_hint, force_cs)
        };
        match out.last_mut() {
            Some((r, last)) if *last == s => r.end = end,
            _ => out.push((i..end, s)),
        }
    }
    out
}
