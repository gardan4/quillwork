//! Partial ("layer") properties: what one `w:rPr` / `w:pPr` / `w:tcPr` says, every field
//! optional, plus the overlay operation that stacks layers.
//!
//! Layers are plain data parsed out of the tree, so the cascade never holds borrows of a
//! [`Document`]. Toggle properties are kept as `Option<bool>` per layer; how layers of
//! different style types combine (XOR) is decided by the resolver, not here.

use std::borrow::Cow;

use crate::wml::theme::{Rgb, ThemeColor, ThemeFont};
use crate::xml::{Document, NodeId, ns::W};

// ---------------------------------------------------------------------- value parsing

/// `w:val` (namespaced lookup).
pub(crate) fn wval<'a>(doc: &'a Document, n: NodeId) -> Option<Cow<'a, str>> {
    wattr(doc, n, "val")
}

/// A `w:`-namespaced attribute.
pub(crate) fn wattr<'a>(doc: &'a Document, n: NodeId, local: &str) -> Option<Cow<'a, str>> {
    doc.attr_ns(n, W, local)
}

/// `ST_OnOff`: absent `w:val` means on.
pub(crate) fn on_off(doc: &Document, n: NodeId) -> bool {
    match wval(doc, n) {
        None => true,
        Some(v) => parse_on_off(&v),
    }
}

pub(crate) fn parse_on_off(v: &str) -> bool {
    let v = v.trim();
    !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
}

/// An integer, tolerating decimals (rounded) as some non-Word producers write them.
pub(crate) fn parse_num(v: &str) -> Option<i64> {
    let v = v.trim();
    if let Ok(i) = v.parse::<i64>() {
        return Some(i);
    }
    let f = v.parse::<f64>().ok()?;
    f.is_finite().then(|| f.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64)
}

/// A measure in `unit` per point (twips: 20, half-points: 2, eighths: 8), accepting a bare
/// number or a universal measure (`12pt`, `1in`, `2.5cm`, `10mm`, `1pc`, `1pi`).
pub(crate) fn parse_measure(v: &str, per_pt: f64) -> Option<i64> {
    let v = v.trim();
    let (num, mul) = if let Some(n) = v.strip_suffix("pt") {
        (n, per_pt)
    } else if let Some(n) = v.strip_suffix("in") {
        (n, per_pt * 72.0)
    } else if let Some(n) = v.strip_suffix("cm") {
        (n, per_pt * 72.0 / 2.54)
    } else if let Some(n) = v.strip_suffix("mm") {
        (n, per_pt * 72.0 / 25.4)
    } else if let Some(n) = v.strip_suffix("pc").or_else(|| v.strip_suffix("pi")) {
        (n, per_pt * 12.0)
    } else {
        return parse_num(v);
    };
    let f = num.trim().parse::<f64>().ok()?;
    let r = (f * mul).round();
    r.is_finite().then(|| r.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
}

fn twips(doc: &Document, n: NodeId, local: &str) -> Option<i32> {
    wattr(doc, n, local).and_then(|v| parse_measure(&v, 20.0)).map(clamp_i32)
}

fn int_attr(doc: &Document, n: NodeId, local: &str) -> Option<i32> {
    wattr(doc, n, local).and_then(|v| parse_num(&v)).map(clamp_i32)
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn hex_byte(v: &str) -> Option<u8> {
    u8::from_str_radix(v.trim(), 16).ok()
}

fn boxed(v: Option<Cow<'_, str>>) -> Option<Box<str>> {
    v.map(|v| v.into_owned().into_boxed_str())
}

/// Child elements of `n` in the `w:` namespace, as (node, local name).
pub(crate) fn w_children<'a>(doc: &'a Document, n: NodeId) -> impl Iterator<Item = (NodeId, &'a str)> + 'a {
    doc.child_elements(n)
        .filter(move |&c| doc.namespace(c) == Some(W))
        .filter_map(move |c| doc.local_name(c).map(|l| (c, l)))
}

// ---------------------------------------------------------------------- colours

/// A colour as written: an explicit value and/or a theme reference (which wins).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ColorSpec {
    /// `Some(None)` = `auto`.
    pub val: Option<Option<Rgb>>,
    pub theme: Option<ThemeColor>,
    pub tint: Option<u8>,
    pub shade: Option<u8>,
}

impl ColorSpec {
    fn read(doc: &Document, n: NodeId, val: &str, theme: &str, tint: &str, shade: &str) -> ColorSpec {
        let v =
            wattr(doc, n, val).map(|v| if v.trim().eq_ignore_ascii_case("auto") { None } else { Rgb::from_hex(&v) });
        ColorSpec {
            val: v,
            theme: wattr(doc, n, theme).and_then(|t| ThemeColor::parse(t.trim())),
            tint: wattr(doc, n, tint).and_then(|t| hex_byte(&t)),
            shade: wattr(doc, n, shade).and_then(|t| hex_byte(&t)),
        }
    }

    pub fn color(doc: &Document, n: NodeId) -> ColorSpec {
        Self::read(doc, n, "color", "themeColor", "themeTint", "themeShade")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ShdSpec {
    pub val: Option<Box<str>>,
    pub color: ColorSpec,
    pub fill: ColorSpec,
}

impl ShdSpec {
    pub fn read(doc: &Document, n: NodeId) -> ShdSpec {
        ShdSpec {
            val: boxed(wval(doc, n)),
            color: ColorSpec::color(doc, n),
            fill: ColorSpec::read(doc, n, "fill", "themeFill", "themeFillTint", "themeFillShade"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct BorderSpec {
    pub val: Option<Box<str>>,
    pub sz: Option<i32>,
    pub space: Option<i32>,
    pub color: ColorSpec,
    pub shadow: Option<bool>,
    pub frame: Option<bool>,
}

impl BorderSpec {
    pub fn read(doc: &Document, n: NodeId) -> BorderSpec {
        BorderSpec {
            val: boxed(wval(doc, n)),
            sz: int_attr(doc, n, "sz"),
            space: int_attr(doc, n, "space"),
            color: ColorSpec::color(doc, n),
            shadow: wattr(doc, n, "shadow").map(|v| parse_on_off(&v)),
            frame: wattr(doc, n, "frame").map(|v| parse_on_off(&v)),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UnderlineSpec {
    pub val: Option<Box<str>>,
    pub color: ColorSpec,
}

// ---------------------------------------------------------------------- run properties

/// Toggle properties, in the order of [`RPr::toggles`].
pub(crate) const TOGGLES: [&str; 13] = [
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "vanish",
];

/// One `w:rFonts` slot: an explicit name and/or a theme font (which wins when it resolves).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FontSlot {
    pub name: Option<Box<str>>,
    pub theme: Option<ThemeFont>,
}

/// Font slot indices.
pub(crate) const ASCII: usize = 0;
pub(crate) const HANSI: usize = 1;
pub(crate) const EAST_ASIA: usize = 2;
pub(crate) const CS: usize = 3;

/// A run-properties layer (`w:rPr`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RPr {
    /// `w:rStyle` (only meaningful in direct formatting).
    pub style: Option<Box<str>>,
    pub fonts: [Option<FontSlot>; 4],
    pub hint: Option<Box<str>>,
    pub toggles: [Option<bool>; 13],
    pub sz: Option<i32>,
    pub sz_cs: Option<i32>,
    pub color: Option<ColorSpec>,
    pub highlight: Option<Box<str>>,
    pub shd: Option<ShdSpec>,
    pub u: Option<UnderlineSpec>,
    pub vert_align: Option<Box<str>>,
    pub position: Option<i32>,
    pub kern: Option<i32>,
    pub spacing: Option<i32>,
    pub w: Option<i32>,
    /// `w:lang` val / eastAsia / bidi.
    pub lang: [Option<Box<str>>; 3],
    pub rtl: Option<bool>,
    pub cs: Option<bool>,
    pub spec_vanish: Option<bool>,
    pub web_hidden: Option<bool>,
    pub no_proof: Option<bool>,
    pub snap_to_grid: Option<bool>,
    pub bdr: Option<BorderSpec>,
    pub em: Option<Box<str>>,
    pub effect: Option<Box<str>>,
}

impl RPr {
    pub fn parse(doc: &Document, rpr: NodeId) -> RPr {
        let mut p = RPr::default();
        for (c, name) in w_children(doc, rpr) {
            if let Some(i) = TOGGLES.iter().position(|t| *t == name) {
                p.toggles[i] = Some(on_off(doc, c));
                continue;
            }
            match name {
                "rStyle" => p.style = boxed(wval(doc, c)),
                "rFonts" => {
                    const SLOTS: [(&str, &str); 4] = [
                        ("ascii", "asciiTheme"),
                        ("hAnsi", "hAnsiTheme"),
                        ("eastAsia", "eastAsiaTheme"),
                        ("cs", "cstheme"),
                    ];
                    for (i, (plain, theme)) in SLOTS.iter().enumerate() {
                        let name = boxed(wattr(doc, c, plain));
                        let theme = wattr(doc, c, theme).and_then(|t| ThemeFont::parse(t.trim()));
                        if name.is_some() || theme.is_some() {
                            p.fonts[i] = Some(FontSlot { name, theme });
                        }
                    }
                    if let Some(h) = wattr(doc, c, "hint") {
                        p.hint = Some(h.into_owned().into_boxed_str());
                    }
                }
                "sz" => p.sz = wval(doc, c).and_then(|v| parse_measure(&v, 2.0)).map(clamp_i32),
                "szCs" => p.sz_cs = wval(doc, c).and_then(|v| parse_measure(&v, 2.0)).map(clamp_i32),
                "color" => p.color = Some(ColorSpec::read(doc, c, "val", "themeColor", "themeTint", "themeShade")),
                "highlight" => p.highlight = boxed(wval(doc, c)),
                "shd" => p.shd = Some(ShdSpec::read(doc, c)),
                "u" => p.u = Some(UnderlineSpec { val: boxed(wval(doc, c)), color: ColorSpec::color(doc, c) }),
                "vertAlign" => p.vert_align = boxed(wval(doc, c)),
                "position" => p.position = wval(doc, c).and_then(|v| parse_measure(&v, 2.0)).map(clamp_i32),
                "kern" => p.kern = wval(doc, c).and_then(|v| parse_measure(&v, 2.0)).map(clamp_i32),
                "spacing" => p.spacing = wval(doc, c).and_then(|v| parse_measure(&v, 20.0)).map(clamp_i32),
                "w" => p.w = wval(doc, c).and_then(|v| parse_num(v.trim_end_matches('%'))).map(clamp_i32),
                "lang" => {
                    p.lang[0] = boxed(wval(doc, c)).or(p.lang[0].take());
                    p.lang[1] = boxed(wattr(doc, c, "eastAsia")).or(p.lang[1].take());
                    p.lang[2] = boxed(wattr(doc, c, "bidi")).or(p.lang[2].take());
                }
                "rtl" => p.rtl = Some(on_off(doc, c)),
                "cs" => p.cs = Some(on_off(doc, c)),
                "specVanish" => p.spec_vanish = Some(on_off(doc, c)),
                "webHidden" => p.web_hidden = Some(on_off(doc, c)),
                "noProof" => p.no_proof = Some(on_off(doc, c)),
                "snapToGrid" => p.snap_to_grid = Some(on_off(doc, c)),
                "bdr" => p.bdr = Some(BorderSpec::read(doc, c)),
                "em" => p.em = boxed(wval(doc, c)),
                "effect" => p.effect = boxed(wval(doc, c)),
                _ => {} // rPrChange, w14 effects, … are not part of the effective view.
            }
        }
        p
    }

    /// Stack `top` over `self`: every field `top` sets wins, toggles included (override, not
    /// XOR: XOR between style types is the resolver's job).
    pub fn overlay(&mut self, top: &RPr) {
        self.overlay_except_toggles(top);
        for (a, b) in self.toggles.iter_mut().zip(top.toggles.iter()) {
            if b.is_some() {
                *a = *b;
            }
        }
    }

    pub fn overlay_except_toggles(&mut self, top: &RPr) {
        fn o<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if b.is_some() {
                a.clone_from(b);
            }
        }
        o(&mut self.style, &top.style);
        for (a, b) in self.fonts.iter_mut().zip(top.fonts.iter()) {
            o(a, b);
        }
        o(&mut self.hint, &top.hint);
        o(&mut self.sz, &top.sz);
        o(&mut self.sz_cs, &top.sz_cs);
        o(&mut self.color, &top.color);
        o(&mut self.highlight, &top.highlight);
        o(&mut self.shd, &top.shd);
        o(&mut self.u, &top.u);
        o(&mut self.vert_align, &top.vert_align);
        o(&mut self.position, &top.position);
        o(&mut self.kern, &top.kern);
        o(&mut self.spacing, &top.spacing);
        o(&mut self.w, &top.w);
        for (a, b) in self.lang.iter_mut().zip(top.lang.iter()) {
            o(a, b);
        }
        o(&mut self.rtl, &top.rtl);
        o(&mut self.cs, &top.cs);
        o(&mut self.spec_vanish, &top.spec_vanish);
        o(&mut self.web_hidden, &top.web_hidden);
        o(&mut self.no_proof, &top.no_proof);
        o(&mut self.snap_to_grid, &top.snap_to_grid);
        o(&mut self.bdr, &top.bdr);
        o(&mut self.em, &top.em);
        o(&mut self.effect, &top.effect);
    }
}

// ---------------------------------------------------------------------- paragraph properties

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct IndSpec {
    pub start: Option<i32>,
    pub end: Option<i32>,
    pub hanging: Option<i32>,
    pub first_line: Option<i32>,
    pub start_chars: Option<i32>,
    pub end_chars: Option<i32>,
    pub hanging_chars: Option<i32>,
    pub first_line_chars: Option<i32>,
}

impl IndSpec {
    fn read(doc: &Document, n: NodeId) -> IndSpec {
        let mut s = IndSpec {
            // `w:start`/`w:end` are the ISO names of `w:left`/`w:right` (both logical).
            start: twips(doc, n, "start").or_else(|| twips(doc, n, "left")),
            end: twips(doc, n, "end").or_else(|| twips(doc, n, "right")),
            hanging: twips(doc, n, "hanging"),
            first_line: twips(doc, n, "firstLine"),
            start_chars: int_attr(doc, n, "startChars").or_else(|| int_attr(doc, n, "leftChars")),
            end_chars: int_attr(doc, n, "endChars").or_else(|| int_attr(doc, n, "rightChars")),
            hanging_chars: int_attr(doc, n, "hangingChars"),
            first_line_chars: int_attr(doc, n, "firstLineChars"),
        };
        // `hanging` wins over `firstLine` in the same element.
        if s.hanging.is_some() {
            s.first_line = None;
        }
        if s.hanging_chars.is_some() {
            s.first_line_chars = None;
        }
        s
    }

    fn overlay(&mut self, top: &IndSpec) {
        fn o(a: &mut Option<i32>, b: Option<i32>) {
            if b.is_some() {
                *a = b;
            }
        }
        o(&mut self.start, top.start);
        o(&mut self.end, top.end);
        o(&mut self.start_chars, top.start_chars);
        o(&mut self.end_chars, top.end_chars);
        // firstLine and hanging are one property written two ways.
        if top.hanging.is_some() {
            self.hanging = top.hanging;
            self.first_line = None;
        } else if top.first_line.is_some() {
            self.first_line = top.first_line;
            self.hanging = None;
        }
        if top.hanging_chars.is_some() {
            self.hanging_chars = top.hanging_chars;
            self.first_line_chars = None;
        } else if top.first_line_chars.is_some() {
            self.first_line_chars = top.first_line_chars;
            self.hanging_chars = None;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SpacingSpec {
    pub before: Option<i32>,
    pub after: Option<i32>,
    pub line: Option<i32>,
    pub line_rule: Option<Box<str>>,
    pub before_autospacing: Option<bool>,
    pub after_autospacing: Option<bool>,
    pub before_lines: Option<i32>,
    pub after_lines: Option<i32>,
}

impl SpacingSpec {
    fn read(doc: &Document, n: NodeId) -> SpacingSpec {
        SpacingSpec {
            before: twips(doc, n, "before"),
            after: twips(doc, n, "after"),
            line: twips(doc, n, "line"),
            line_rule: boxed(wattr(doc, n, "lineRule")),
            before_autospacing: wattr(doc, n, "beforeAutospacing").map(|v| parse_on_off(&v)),
            after_autospacing: wattr(doc, n, "afterAutospacing").map(|v| parse_on_off(&v)),
            before_lines: int_attr(doc, n, "beforeLines"),
            after_lines: int_attr(doc, n, "afterLines"),
        }
    }

    fn overlay(&mut self, top: &SpacingSpec) {
        fn o<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if b.is_some() {
                a.clone_from(b);
            }
        }
        o(&mut self.before, &top.before);
        o(&mut self.after, &top.after);
        o(&mut self.line, &top.line);
        o(&mut self.line_rule, &top.line_rule);
        o(&mut self.before_autospacing, &top.before_autospacing);
        o(&mut self.after_autospacing, &top.after_autospacing);
        o(&mut self.before_lines, &top.before_lines);
        o(&mut self.after_lines, &top.after_lines);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TabSpec {
    pub val: Box<str>,
    pub pos: i32,
    pub leader: Option<Box<str>>,
}

/// Paragraph boolean properties, in the order of [`PPr::flags`].
pub(crate) const PARA_FLAGS: [&str; 12] = [
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "widowControl",
    "contextualSpacing",
    "bidi",
    "suppressAutoHyphens",
    "suppressLineNumbers",
    "mirrorIndents",
    "snapToGrid",
    "wordWrap",
    "suppressOverlap",
];

/// Paragraph border sides, in the order of [`PPr::borders`].
pub(crate) const PARA_BORDERS: [&str; 6] = ["top", "left", "bottom", "right", "between", "bar"];

/// A paragraph-properties layer (`w:pPr`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PPr {
    /// `w:pStyle` (only meaningful in direct formatting).
    pub style: Option<Box<str>>,
    pub jc: Option<Box<str>>,
    pub ind: IndSpec,
    pub spacing: SpacingSpec,
    pub flags: [Option<bool>; 12],
    pub outline_lvl: Option<i32>,
    pub borders: [Option<BorderSpec>; 6],
    pub shd: Option<ShdSpec>,
    /// Tab stops of this layer, `clear` entries included (they cancel inherited stops).
    pub tabs: Vec<TabSpec>,
    pub num_id: Option<i32>,
    pub ilvl: Option<i32>,
    pub text_alignment: Option<Box<str>>,
    /// `w:pPr/w:rPr`: the paragraph mark's run properties (direct formatting only).
    pub mark: Option<Box<RPr>>,
}

impl PPr {
    pub fn parse(doc: &Document, ppr: NodeId) -> PPr {
        let mut p = PPr::default();
        for (c, name) in w_children(doc, ppr) {
            if let Some(i) = PARA_FLAGS.iter().position(|t| *t == name) {
                p.flags[i] = Some(on_off(doc, c));
                continue;
            }
            match name {
                "pStyle" => p.style = boxed(wval(doc, c)),
                "jc" => p.jc = boxed(wval(doc, c)),
                "ind" => p.ind = IndSpec::read(doc, c),
                "spacing" => p.spacing = SpacingSpec::read(doc, c),
                "outlineLvl" => p.outline_lvl = wval(doc, c).and_then(|v| parse_num(&v)).map(clamp_i32),
                "pBdr" => {
                    for (b, side) in w_children(doc, c) {
                        let side = match side {
                            "start" => "left",
                            "end" => "right",
                            s => s,
                        };
                        if let Some(i) = PARA_BORDERS.iter().position(|s| *s == side) {
                            p.borders[i] = Some(BorderSpec::read(doc, b));
                        }
                    }
                }
                "shd" => p.shd = Some(ShdSpec::read(doc, c)),
                "tabs" => {
                    for (t, tn) in w_children(doc, c) {
                        if tn != "tab" {
                            continue;
                        }
                        let Some(pos) = twips(doc, t, "pos") else { continue };
                        let val = boxed(wval(doc, t)).unwrap_or_else(|| "left".into());
                        p.tabs.retain(|x| x.pos != pos);
                        p.tabs.push(TabSpec { val, pos, leader: boxed(wattr(doc, t, "leader")) });
                    }
                }
                "numPr" => {
                    for (x, xn) in w_children(doc, c) {
                        match xn {
                            "numId" => p.num_id = wval(doc, x).and_then(|v| parse_num(&v)).map(clamp_i32),
                            "ilvl" => p.ilvl = wval(doc, x).and_then(|v| parse_num(&v)).map(clamp_i32),
                            _ => {}
                        }
                    }
                }
                "textAlignment" => p.text_alignment = boxed(wval(doc, c)),
                "rPr" => p.mark = Some(Box::new(RPr::parse(doc, c))),
                _ => {} // sectPr, pPrChange, framePr, … are not paragraph formatting we resolve.
            }
        }
        p
    }

    /// Stack `top` over `self`. Tabs merge by position (a `clear` stop is kept so it can
    /// cancel a stop from a lower layer; [`PPr::final_tabs`] drops it).
    pub fn overlay(&mut self, top: &PPr) {
        fn o<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if b.is_some() {
                a.clone_from(b);
            }
        }
        o(&mut self.style, &top.style);
        o(&mut self.jc, &top.jc);
        self.ind.overlay(&top.ind);
        self.spacing.overlay(&top.spacing);
        for (a, b) in self.flags.iter_mut().zip(top.flags.iter()) {
            o(a, b);
        }
        o(&mut self.outline_lvl, &top.outline_lvl);
        for (a, b) in self.borders.iter_mut().zip(top.borders.iter()) {
            o(a, b);
        }
        o(&mut self.shd, &top.shd);
        for t in &top.tabs {
            self.tabs.retain(|x| x.pos != t.pos);
            self.tabs.push(t.clone());
        }
        o(&mut self.num_id, &top.num_id);
        o(&mut self.ilvl, &top.ilvl);
        o(&mut self.text_alignment, &top.text_alignment);
        o(&mut self.mark, &top.mark);
    }

    /// Effective tab stops: `clear` entries dropped, sorted by position.
    pub fn final_tabs(&self) -> Vec<&TabSpec> {
        let mut v: Vec<&TabSpec> = self.tabs.iter().filter(|t| &*t.val != "clear").collect();
        v.sort_by_key(|t| t.pos);
        v
    }
}

// ---------------------------------------------------------------------- cell properties

/// Cell border sides, in the order of [`TcPr::borders`].
pub(crate) const CELL_BORDERS: [&str; 8] = ["top", "left", "bottom", "right", "insideH", "insideV", "tl2br", "tr2bl"];

/// A table-cell-properties layer (`w:tcPr`), the parts that affect how a cell looks.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TcPr {
    pub shd: Option<ShdSpec>,
    pub v_align: Option<Box<str>>,
    pub borders: [Option<BorderSpec>; 8],
    pub no_wrap: Option<bool>,
    pub text_direction: Option<Box<str>>,
}

impl TcPr {
    pub fn parse(doc: &Document, tcpr: NodeId) -> TcPr {
        let mut p = TcPr::default();
        for (c, name) in w_children(doc, tcpr) {
            match name {
                "shd" => p.shd = Some(ShdSpec::read(doc, c)),
                "vAlign" => p.v_align = boxed(wval(doc, c)),
                "noWrap" => p.no_wrap = Some(on_off(doc, c)),
                "textDirection" => p.text_direction = boxed(wval(doc, c)),
                "tcBorders" => {
                    for (b, side) in w_children(doc, c) {
                        let side = match side {
                            "start" => "left",
                            "end" => "right",
                            s => s,
                        };
                        if let Some(i) = CELL_BORDERS.iter().position(|s| *s == side) {
                            p.borders[i] = Some(BorderSpec::read(doc, b));
                        }
                    }
                }
                _ => {}
            }
        }
        p
    }

    pub fn overlay(&mut self, top: &TcPr) {
        fn o<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if b.is_some() {
                a.clone_from(b);
            }
        }
        o(&mut self.shd, &top.shd);
        o(&mut self.v_align, &top.v_align);
        for (a, b) in self.borders.iter_mut().zip(top.borders.iter()) {
            o(a, b);
        }
        o(&mut self.no_wrap, &top.no_wrap);
        o(&mut self.text_direction, &top.text_direction);
    }
}
