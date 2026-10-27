//! Effective (fully resolved) properties: plain data the view model embeds.
//!
//! Every field is `None` when nothing in the cascade sets it; the viewer then uses the
//! application default (e.g. 10 pt Times New Roman, no spacing). Colours are `RRGGBB` or
//! `auto`. Measurements keep Word's units: sizes in half-points, lengths in twips,
//! border widths in eighths of a point.

use serde::{Deserialize, Serialize};

use super::script::Script;

/// Declares a struct whose fields are all `Option`, serialised camelCase without the `None`s.
macro_rules! opt_struct {
    ($(#[$m:meta])* pub struct $name:ident { $($(#[$fm:meta])* $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            $($(#[$fm])* #[serde(default, skip_serializing_if = "Option::is_none")] pub $f: Option<$t>,)*
        }
    };
}

opt_struct! {
    /// Resolved `w:rFonts` (theme fonts replaced by typefaces).
    pub struct Fonts {
        ascii: String,
        h_ansi: String,
        east_asia: String,
        cs: String,
        /// `w:hint` (`default`, `eastAsia`, `cs`).
        hint: String,
    }
}

impl Fonts {
    pub fn is_empty(&self) -> bool {
        *self == Fonts::default()
    }

    /// The typeface for a slot.
    pub fn for_script(&self, s: Script) -> Option<&str> {
        match s {
            Script::Ascii => self.ascii.as_deref(),
            Script::HAnsi => self.h_ansi.as_deref(),
            Script::EastAsia => self.east_asia.as_deref(),
            Script::ComplexScript => self.cs.as_deref(),
        }
    }
}

opt_struct! {
    /// `w:shd`, colours resolved.
    pub struct Shading {
        /// Pattern (`clear`, `solid`, `pct10`, …).
        val: String,
        /// Pattern colour.
        color: String,
        /// Background fill.
        fill: String,
    }
}

opt_struct! {
    /// One border (`w:top`, `w:bdr`, …).
    pub struct Border {
        val: String,
        /// Width in eighths of a point.
        sz: i32,
        /// Spacing in points.
        space: i32,
        color: String,
        shadow: bool,
        frame: bool,
    }
}

opt_struct! {
    /// `w:u`.
    pub struct Underline {
        val: String,
        color: String,
    }
}

opt_struct! {
    /// `w:lang`.
    pub struct Lang {
        val: String,
        east_asia: String,
        bidi: String,
    }
}

opt_struct! {
    /// Effective run properties.
    pub struct EffectiveRun {
        /// The character style in effect (`w:rStyle`), if it exists.
        style: String,
        fonts: Fonts,
        /// The typeface for this run's text (by [`EffectiveRun::script`]).
        font: String,
        /// The slot this run's text is drawn with.
        script: Script,
        bold: bool,
        bold_cs: bool,
        italic: bool,
        italic_cs: bool,
        caps: bool,
        small_caps: bool,
        strike: bool,
        dstrike: bool,
        outline: bool,
        shadow: bool,
        emboss: bool,
        imprint: bool,
        vanish: bool,
        /// `w:sz`, half-points.
        size: u32,
        /// `w:szCs`, half-points.
        size_cs: u32,
        color: String,
        /// `w:highlight` name (`yellow`, …; `none` kept).
        highlight: String,
        shading: Shading,
        underline: Underline,
        /// `superscript` / `subscript` / `baseline`.
        vert_align: String,
        /// Raise / lower, half-points.
        position: i32,
        /// Kerning threshold, half-points.
        kern: u32,
        /// Character spacing, twips.
        spacing: i32,
        /// Horizontal scale, percent.
        scale: u32,
        lang: Lang,
        rtl: bool,
        cs: bool,
        spec_vanish: bool,
        web_hidden: bool,
        no_proof: bool,
        snap_to_grid: bool,
        border: Border,
        emphasis_mark: String,
        effect: String,
    }
}

impl EffectiveRun {
    /// Bold as shown for a slot (`bCs` for complex script).
    pub fn bold_for(&self, s: Script) -> bool {
        if s.is_complex() { self.bold_cs } else { self.bold }.unwrap_or(false)
    }

    /// Italic as shown for a slot (`iCs` for complex script).
    pub fn italic_for(&self, s: Script) -> bool {
        if s.is_complex() { self.italic_cs } else { self.italic }.unwrap_or(false)
    }

    /// Size in half-points for a slot (`szCs` for complex script).
    pub fn size_for(&self, s: Script) -> Option<u32> {
        if s.is_complex() { self.size_cs } else { self.size }
    }

    /// Typeface for a slot.
    pub fn font_for(&self, s: Script) -> Option<&str> {
        self.fonts.as_ref().and_then(|f| f.for_script(s))
    }
}

opt_struct! {
    /// `w:ind`, in twips. `start`/`end` are logical (`w:left`/`w:right` are read as the same).
    pub struct Indent {
        start: i32,
        end: i32,
        hanging: i32,
        first_line: i32,
        start_chars: i32,
        end_chars: i32,
        hanging_chars: i32,
        first_line_chars: i32,
    }
}

opt_struct! {
    /// `w:spacing`, in twips (`line` in 240ths of a line when `lineRule` is `auto`).
    pub struct Spacing {
        before: i32,
        after: i32,
        line: i32,
        line_rule: String,
        before_autospacing: bool,
        after_autospacing: bool,
        before_lines: i32,
        after_lines: i32,
    }
}

opt_struct! {
    /// `w:pBdr`.
    pub struct ParagraphBorders {
        top: Border,
        left: Border,
        bottom: Border,
        right: Border,
        between: Border,
        bar: Border,
    }
}

/// One tab stop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabStop {
    /// `left`, `center`, `right`, `decimal`, `bar`, `num`, `start`, `end`.
    pub val: String,
    /// Position in twips.
    pub pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
}

/// The list a paragraph belongs to (`w:numPr`, from the paragraph or its style).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberingRef {
    pub num_id: i32,
    pub ilvl: u8,
}

/// Context the run resolver needs from the paragraph (not serialised).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParaCtx {
    pub style: Option<Box<str>>,
    pub table: super::table::TableCtx,
}

/// Effective paragraph properties.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveParagraph {
    /// The paragraph style in effect (the default paragraph style when `w:pStyle` is absent
    /// or names a style that does not exist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Logical alignment: `start`, `center`, `end`, `both`, `distribute`, … (`left` and
    /// `right` are read as `start` and `end`; with [`EffectiveParagraph::bidi`] start is the
    /// right edge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent: Option<Indent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Spacing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_next: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_lines: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_break_before: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widow_control: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contextual_spacing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bidi: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_auto_hyphens: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_line_numbers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_indents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snap_to_grid: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_wrap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_overlap: Option<bool>,
    /// 0–8 = heading levels 1–9; 9 = body text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borders: Option<ParagraphBorders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shading: Option<Shading>,
    /// Custom tab stops, sorted by position (`clear` stops already applied).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabStop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numbering: Option<NumberingRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_alignment: Option<String>,
    #[serde(skip)]
    pub(crate) ctx: ParaCtx,
}

opt_struct! {
    /// `w:tcBorders`.
    pub struct CellBorders {
        top: Border,
        left: Border,
        bottom: Border,
        right: Border,
        inside_h: Border,
        inside_v: Border,
        tl2br: Border,
        tr2bl: Border,
    }
}

opt_struct! {
    /// Effective cell properties from the table style (whole table + conditional formats)
    /// and the cell's own `w:tcPr`.
    pub struct EffectiveCell {
        shading: Shading,
        v_align: String,
        borders: CellBorders,
        no_wrap: bool,
        text_direction: String,
    }
}
