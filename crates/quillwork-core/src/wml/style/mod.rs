//! Style resolution: effective paragraph, run and cell properties (`docs/DESIGN.md` §4.3).
//!
//! The cascade, lowest first:
//!
//! ```text
//! docDefaults (rPrDefault / pPrDefault)
//!  → table style (whole table, then the conditional formats tblLook turns on for the cell)
//!  → numbering level pPr            (paragraphs; level rPr only formats the list label)
//!  → paragraph style (basedOn chain)
//!  → character style (basedOn chain)
//!  → direct formatting
//! ```
//!
//! Word quirks we follow:
//!
//! * **Toggle properties** (`b`, `i`, `caps`, `vanish`, …): inside one basedOn chain the
//!   nearest setting wins; across style *types* (table style, paragraph style, character
//!   style) the chain results are XORed, so a bold character style inside a bold paragraph
//!   style gives non-bold text. Direct formatting overrides. A character style linked to the
//!   paragraph's own style (`Heading1Char` in `Heading1`) overrides instead of toggling.
//! * **Table style vs. the default paragraph style**: the table style overrides what the
//!   default paragraph style (usually *Normal*) and docDefaults say, but not what other
//!   paragraph styles say. So the default style's part of a basedOn chain is applied before
//!   the table style and the rest of the chain after it.
//! * **Numbering indentation**: numbering referenced from the paragraph style sits below the
//!   style (the style's own `w:ind` wins); numbering set directly on the paragraph sits above
//!   the style (its `w:ind` wins), below direct formatting.
//!
//! Style-level results are cached per (paragraph style, character style, numbering level,
//! table style + conditional formats); direct formatting is read from the element on every
//! call, so editing an element needs no invalidation. After changing styles, numbering or
//! settings, call the matching `set_*` (which clears the cache) or [`StyleSheet::invalidate`].

mod output;
mod props;
mod script;
mod table;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

pub use output::{
    Border, CellBorders, EffectiveCell, EffectiveParagraph, EffectiveRun, Fonts, Indent, Lang, NumberingRef,
    ParagraphBorders, Shading, Spacing, TabStop, Underline,
};
pub use script::{Script, script_of, script_of_text, script_segments};
pub use table::{CondSet, CondType, TableCtx, TblLook};

use output::ParaCtx;
use props::{
    BorderSpec, CELL_BORDERS, ColorSpec, PARA_BORDERS, PPr, RPr, ShdSpec, TOGGLES, TcPr, parse_num, w_children, wattr,
    wval,
};

use crate::wml::theme::{ColorMapping, SchemeColor, Theme};
use crate::xml::{Document, NodeId, ns::W};
use crate::{Error, Result};

/// Longest basedOn chain we follow (Word itself stops far earlier).
const MAX_CHAIN: usize = 64;

/// `w:style/@w:type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StyleKind {
    Paragraph,
    Character,
    Table,
    Numbering,
}

impl StyleKind {
    fn parse(s: Option<&str>) -> StyleKind {
        match s {
            Some("character") => StyleKind::Character,
            Some("table") => StyleKind::Table,
            Some("numbering") => StyleKind::Numbering,
            _ => StyleKind::Paragraph,
        }
    }
}

/// The three property layers one style (or one conditional format) carries.
#[derive(Debug, Clone, Default, PartialEq)]
struct Layer {
    ppr: PPr,
    rpr: RPr,
    tcpr: TcPr,
}

impl Layer {
    fn overlay(&mut self, top: &Layer) {
        self.ppr.overlay(&top.ppr);
        self.rpr.overlay(&top.rpr);
        self.tcpr.overlay(&top.tcpr);
    }

    fn read(doc: &Document, n: NodeId) -> Layer {
        let mut l = Layer::default();
        for (c, name) in w_children(doc, n) {
            match name {
                "pPr" => l.ppr = PPr::parse(doc, c),
                "rPr" => l.rpr = RPr::parse(doc, c),
                "tcPr" => l.tcpr = TcPr::parse(doc, c),
                _ => {}
            }
        }
        l
    }
}

/// One `w:style` definition.
#[derive(Debug, Clone)]
pub struct Style {
    id: String,
    name: Option<String>,
    kind: StyleKind,
    based_on: Option<String>,
    link: Option<String>,
    next: Option<String>,
    is_default: bool,
    custom: bool,
    hidden: bool,
    q_format: bool,
    ui_priority: Option<i32>,
    aliases: Vec<String>,
    own: Layer,
    conds: Vec<(CondType, Layer)>,
    row_band: Option<u32>,
    col_band: Option<u32>,
}

impl Style {
    pub fn id(&self) -> &str {
        &self.id
    }
    /// `w:name` (built-in styles use the English name, e.g. `heading 1`, whatever the UI language).
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    pub fn kind(&self) -> StyleKind {
        self.kind
    }
    pub fn based_on(&self) -> Option<&str> {
        self.based_on.as_deref()
    }
    /// `w:link`: the linked character style of a paragraph style, or vice versa.
    pub fn linked(&self) -> Option<&str> {
        self.link.as_deref()
    }
    pub fn next(&self) -> Option<&str> {
        self.next.as_deref()
    }
    /// `w:default="1"`: the default style of its kind.
    pub fn is_default(&self) -> bool {
        self.is_default
    }
    pub fn is_custom(&self) -> bool {
        self.custom
    }
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }
    pub fn is_quick_style(&self) -> bool {
        self.q_format
    }
    pub fn ui_priority(&self) -> Option<i32> {
        self.ui_priority
    }
    /// `w:aliases`, split on commas.
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }
    /// The `w:numPr/w:numId` this style sets itself (not inherited).
    pub fn num_id(&self) -> Option<i32> {
        self.own.ppr.num_id
    }
    /// The `w:pPr/w:outlineLvl` this style sets itself (not inherited).
    pub fn outline_level(&self) -> Option<i32> {
        self.own.ppr.outline_lvl
    }

    fn parse(doc: &Document, n: NodeId) -> Option<Style> {
        let id = wattr(doc, n, "styleId")?.into_owned();
        let child_val = |local: &str| doc.find_child(n, W, local).and_then(|c| wval(doc, c)).map(|v| v.into_owned());
        let flag = |local: &str| doc.find_child(n, W, local).is_some_and(|c| props::on_off(doc, c));
        let mut s = Style {
            kind: StyleKind::parse(wattr(doc, n, "type").as_deref()),
            name: child_val("name"),
            based_on: child_val("basedOn").filter(|b| *b != id),
            link: child_val("link"),
            next: child_val("next"),
            is_default: wattr(doc, n, "default").is_some_and(|v| props::parse_on_off(&v)),
            custom: wattr(doc, n, "customStyle").is_some_and(|v| props::parse_on_off(&v)),
            hidden: flag("hidden") || flag("semiHidden"),
            q_format: flag("qFormat"),
            ui_priority: child_val("uiPriority").and_then(|v| parse_num(&v)).map(|v| v as i32),
            aliases: child_val("aliases")
                .map(|a| a.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            own: Layer::read(doc, n),
            conds: Vec::new(),
            row_band: None,
            col_band: None,
            id,
        };
        for (c, name) in w_children(doc, n) {
            match name {
                "tblStylePr" => {
                    if let Some(t) = wattr(doc, c, "type").and_then(|t| CondType::parse(t.trim())) {
                        s.conds.push((t, Layer::read(doc, c)));
                    }
                }
                "tblPr" => {
                    for (b, bn) in w_children(doc, c) {
                        let v = wval(doc, b).and_then(|v| parse_num(&v)).map(|v| v.clamp(1, 1 << 16) as u32);
                        match bn {
                            "tblStyleRowBandSize" => s.row_band = v,
                            "tblStyleColBandSize" => s.col_band = v,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Some(s)
    }
}

/// A style's basedOn chain, flattened once at parse time.
#[derive(Debug, Clone, Default)]
struct Flat {
    /// Style indices, root first.
    chain: Vec<usize>,
    /// The whole chain merged (nearest wins).
    all: Layer,
    /// The chain up to and including the default paragraph style, when it is in the chain.
    default_prefix: Option<Layer>,
    /// The chain after the default paragraph style (the whole chain if it is not in it).
    rest: Layer,
    /// Conditional formats merged along the chain, by `CondType as usize`.
    conds: [Option<Layer>; 13],
    row_band: Option<u32>,
    col_band: Option<u32>,
}

/// One numbering level's formatting.
#[derive(Debug, Clone, Default)]
struct Level {
    ppr: PPr,
    rpr: RPr,
}

#[derive(Debug, Clone, Default)]
struct Numbering {
    /// numId → (abstractNumId, level overrides).
    nums: HashMap<i32, (i32, HashMap<i32, Level>)>,
    /// abstractNumId → (levels, numStyleLink).
    abstracts: HashMap<i32, (HashMap<i32, Level>, Option<String>)>,
}

impl Numbering {
    fn parse(doc: &Document) -> Numbering {
        let mut n = Numbering::default();
        let level = |l: NodeId| -> Option<(i32, Level)> {
            let ilvl = wattr(doc, l, "ilvl").and_then(|v| parse_num(&v))? as i32;
            let mut lv = Level::default();
            for (c, name) in w_children(doc, l) {
                match name {
                    "pPr" => lv.ppr = PPr::parse(doc, c),
                    "rPr" => lv.rpr = RPr::parse(doc, c),
                    _ => {}
                }
            }
            Some((ilvl, lv))
        };
        for (c, name) in w_children(doc, doc.root()) {
            match name {
                "abstractNum" => {
                    let Some(id) = wattr(doc, c, "abstractNumId").and_then(|v| parse_num(&v)) else { continue };
                    let mut levels = HashMap::new();
                    let mut link = None;
                    for (x, xn) in w_children(doc, c) {
                        match xn {
                            "lvl" => {
                                if let Some((i, lv)) = level(x) {
                                    levels.entry(i).or_insert(lv);
                                }
                            }
                            "numStyleLink" => link = wval(doc, x).map(|v| v.into_owned()),
                            _ => {}
                        }
                    }
                    n.abstracts.entry(id as i32).or_insert((levels, link));
                }
                "num" => {
                    let Some(id) = wattr(doc, c, "numId").and_then(|v| parse_num(&v)) else { continue };
                    let mut abs = None;
                    let mut overrides = HashMap::new();
                    for (x, xn) in w_children(doc, c) {
                        match xn {
                            "abstractNumId" => abs = wval(doc, x).and_then(|v| parse_num(&v)),
                            "lvlOverride" => {
                                if let Some(l) = doc.find_child(x, W, "lvl")
                                    && let Some((_, lv)) = level(l)
                                {
                                    let i = wattr(doc, x, "ilvl").and_then(|v| parse_num(&v)).unwrap_or(0);
                                    overrides.insert(i as i32, lv);
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(a) = abs {
                        n.nums.entry(id as i32).or_insert((a as i32, overrides));
                    }
                }
                _ => {}
            }
        }
        n
    }
}

/// Settings that affect resolution.
#[derive(Debug, Clone, Default)]
struct Settings {
    color_map: ColorMapping,
    /// `w:themeFontLang` val / eastAsia / bidi.
    theme_font_lang: [Option<String>; 3],
    default_tab_stop: Option<i32>,
}

type TableKey = Option<(usize, CondSet)>;
type ParaKey = (Option<usize>, TableKey, Option<(i32, i32, bool)>);
type RunKey = (Option<usize>, Option<usize>, TableKey);

/// Parsed styles (+ theme, numbering, settings) and the resolver over them.
pub struct StyleSheet {
    styles: Vec<Style>,
    flats: Vec<Flat>,
    by_id: HashMap<String, usize>,
    defaults_ppr: PPr,
    defaults_rpr: RPr,
    default_para: Option<usize>,
    default_char: Option<usize>,
    default_table: Option<usize>,
    theme: Option<Theme>,
    numbering: Numbering,
    settings: Settings,
    para_cache: Mutex<HashMap<ParaKey, Arc<PPr>>>,
    run_cache: Mutex<HashMap<RunKey, Arc<RPr>>>,
}

impl std::fmt::Debug for StyleSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StyleSheet")
            .field("styles", &self.styles.len())
            .field("theme", &self.theme.is_some())
            .field("default_paragraph_style", &self.default_paragraph_style_id())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl StyleSheet {
    /// Parse `word/styles.xml`, with `word/theme/theme1.xml` when the package has one.
    pub fn parse(styles: &Document, theme: Option<&Document>) -> Result<StyleSheet> {
        let root = styles.root();
        if !styles.is(root, W, "styles") {
            return Err(Error::Style(format!(
                "styles part root is <{}>, expected <w:styles>",
                styles.name(root).unwrap_or("?")
            )));
        }
        let theme = theme.map(Theme::parse).transpose()?;
        let mut sheet = StyleSheet {
            styles: Vec::new(),
            flats: Vec::new(),
            by_id: HashMap::new(),
            defaults_ppr: PPr::default(),
            defaults_rpr: RPr::default(),
            default_para: None,
            default_char: None,
            default_table: None,
            theme,
            numbering: Numbering::default(),
            settings: Settings::default(),
            para_cache: Mutex::new(HashMap::new()),
            run_cache: Mutex::new(HashMap::new()),
        };
        for (c, name) in w_children(styles, root) {
            match name {
                "docDefaults" => {
                    for (d, dn) in w_children(styles, c) {
                        let inner = |local| styles.find_child(d, W, local);
                        match dn {
                            "rPrDefault" => {
                                if let Some(r) = inner("rPr") {
                                    sheet.defaults_rpr = RPr::parse(styles, r);
                                }
                            }
                            "pPrDefault" => {
                                if let Some(p) = inner("pPr") {
                                    sheet.defaults_ppr = PPr::parse(styles, p);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "style" => {
                    if let Some(s) = Style::parse(styles, c) {
                        // Duplicate ids: the first definition wins.
                        if !sheet.by_id.contains_key(&s.id) {
                            sheet.by_id.insert(s.id.clone(), sheet.styles.len());
                            sheet.styles.push(s);
                        }
                    }
                }
                _ => {}
            }
        }
        let first_default = |k: StyleKind| sheet.styles.iter().position(|s| s.kind == k && s.is_default);
        sheet.default_para = first_default(StyleKind::Paragraph)
            .or_else(|| sheet.by_id.get("Normal").copied().filter(|&i| sheet.styles[i].kind == StyleKind::Paragraph));
        sheet.default_char = first_default(StyleKind::Character);
        sheet.default_table = first_default(StyleKind::Table);
        sheet.flats = (0..sheet.styles.len()).map(|i| sheet.flatten(i)).collect();
        Ok(sheet)
    }

    /// Add (or replace) `word/numbering.xml`, for numbering-level indentation.
    pub fn set_numbering(&mut self, numbering: &Document) -> Result<()> {
        if !numbering.is(numbering.root(), W, "numbering") {
            return Err(Error::Style("numbering part root is not <w:numbering>".into()));
        }
        self.numbering = Numbering::parse(numbering);
        self.invalidate();
        Ok(())
    }

    /// Add (or replace) `word/settings.xml`: `w:clrSchemeMapping`, `w:themeFontLang`,
    /// `w:defaultTabStop`.
    pub fn set_settings(&mut self, settings: &Document) -> Result<()> {
        if !settings.is(settings.root(), W, "settings") {
            return Err(Error::Style("settings part root is not <w:settings>".into()));
        }
        let mut s = Settings::default();
        for (c, name) in w_children(settings, settings.root()) {
            match name {
                "themeFontLang" => {
                    s.theme_font_lang = [
                        wval(settings, c).map(|v| v.into_owned()),
                        wattr(settings, c, "eastAsia").map(|v| v.into_owned()),
                        wattr(settings, c, "bidi").map(|v| v.into_owned()),
                    ]
                }
                "clrSchemeMapping" => {
                    let get = |a: &str, d: SchemeColor| {
                        wattr(settings, c, a).and_then(|v| SchemeColor::from_wml(v.trim())).unwrap_or(d)
                    };
                    let d = ColorMapping::default();
                    s.color_map = ColorMapping {
                        background1: get("bg1", d.background1),
                        text1: get("t1", d.text1),
                        background2: get("bg2", d.background2),
                        text2: get("t2", d.text2),
                    };
                }
                "defaultTabStop" => {
                    s.default_tab_stop =
                        wval(settings, c).and_then(|v| props::parse_measure(&v, 20.0)).map(|v| v as i32)
                }
                _ => {}
            }
        }
        self.settings = s;
        self.invalidate();
        Ok(())
    }

    /// Drop every cached resolution. Results stay correct without calling this unless the
    /// style data itself is swapped out behind the sheet's back.
    pub fn invalidate(&self) {
        lock(&self.para_cache).clear();
        lock(&self.run_cache).clear();
    }

    /// (paragraph entries, run entries) in the cache.
    pub fn cache_len(&self) -> (usize, usize) {
        (lock(&self.para_cache).len(), lock(&self.run_cache).len())
    }

    // ------------------------------------------------------------------ lookup

    pub fn theme(&self) -> Option<&Theme> {
        self.theme.as_ref()
    }

    /// `w:defaultTabStop` from settings, twips.
    pub fn default_tab_stop(&self) -> Option<i32> {
        self.settings.default_tab_stop
    }

    /// All styles, in document order.
    pub fn styles(&self) -> impl Iterator<Item = &Style> {
        self.styles.iter()
    }

    pub fn style_by_id(&self, id: &str) -> Option<&Style> {
        self.by_id.get(id).map(|&i| &self.styles[i])
    }

    /// The style used by paragraphs without `w:pStyle` (`w:default="1"`, else `Normal`).
    pub fn default_paragraph_style_id(&self) -> Option<&str> {
        self.default_para.map(|i| self.styles[i].id.as_str())
    }

    pub fn default_character_style_id(&self) -> Option<&str> {
        self.default_char.map(|i| self.styles[i].id.as_str())
    }

    /// The style used by tables without `w:tblStyle` (usually `TableNormal`).
    pub fn default_table_style_id(&self) -> Option<&str> {
        self.default_table.map(|i| self.styles[i].id.as_str())
    }

    /// The basedOn chain of a style, the style itself first. Cycle-safe.
    pub fn style_chain(&self, id: &str) -> Vec<&Style> {
        self.by_id
            .get(id)
            .map(|&i| self.flats[i].chain.iter().rev().map(|&j| &self.styles[j]).collect())
            .unwrap_or_default()
    }

    /// Find a style by display name, as a user or an agent would type it: case-insensitive
    /// `w:name`, then `w:aliases`, then the style id, then both with spaces/punctuation
    /// ignored, then common localised names of built-in styles ("Kop 1", "Überschrift 1",
    /// "Titre 1" → `heading 1`; "Standaard" → `Normal`, …).
    pub fn style_by_name(&self, name: &str) -> Option<&Style> {
        self.find_by_name(name, None)
    }

    /// [`StyleSheet::style_by_name`], restricted to one kind.
    pub fn style_by_name_of_kind(&self, name: &str, kind: StyleKind) -> Option<&Style> {
        self.find_by_name(name, Some(kind))
    }

    fn find_by_name(&self, name: &str, kind: Option<StyleKind>) -> Option<&Style> {
        let q = name.trim();
        if q.is_empty() {
            return None;
        }
        let ok = |s: &&Style| kind.is_none_or(|k| s.kind == k);
        let eq = |a: &str| a.eq_ignore_ascii_case(q) || a.to_lowercase() == q.to_lowercase();
        let squash =
            |s: &str| -> String { s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect() };
        let sq = squash(q);
        let found = self
            .styles
            .iter()
            .filter(ok)
            .find(|s| s.name.as_deref().is_some_and(eq))
            .or_else(|| self.styles.iter().filter(ok).find(|s| s.aliases.iter().any(|a| eq(a))))
            .or_else(|| self.styles.iter().filter(ok).find(|s| eq(&s.id)))
            .or_else(|| {
                (!sq.is_empty()).then(|| {
                    self.styles
                        .iter()
                        .filter(ok)
                        .find(|s| s.name.as_deref().is_some_and(|n| squash(n) == sq) || squash(&s.id) == sq)
                })?
            });
        if found.is_some() {
            return found;
        }
        let canonical = builtin_name(&q.to_lowercase())?;
        self.styles.iter().filter(ok).find(|s| s.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&canonical)))
    }

    pub(crate) fn band_sizes(&self, table_style: &str) -> (Option<u32>, Option<u32>) {
        self.by_id.get(table_style).map(|&i| (self.flats[i].row_band, self.flats[i].col_band)).unwrap_or((None, None))
    }

    fn index_of_kind(&self, id: Option<&str>, kind: StyleKind) -> Option<usize> {
        let i = *self.by_id.get(id?)?;
        (self.styles[i].kind == kind).then_some(i)
    }

    fn para_index(&self, direct: Option<&str>) -> Option<usize> {
        self.index_of_kind(direct, StyleKind::Paragraph).or(self.default_para)
    }

    fn table_key(&self, ctx: &TableCtx) -> TableKey {
        self.index_of_kind(ctx.style.as_deref(), StyleKind::Table).map(|i| (i, ctx.cond))
    }

    // ------------------------------------------------------------------ flattening

    fn chain_of(&self, i: usize) -> Vec<usize> {
        let mut chain = vec![i];
        let mut seen: HashSet<usize> = HashSet::from([i]);
        let mut cur = i;
        while let Some(parent) = self.styles[cur].based_on.as_deref().and_then(|b| self.by_id.get(b)).copied() {
            if !seen.insert(parent) || chain.len() >= MAX_CHAIN || self.styles[parent].kind != self.styles[i].kind {
                break;
            }
            chain.push(parent);
            cur = parent;
        }
        chain.reverse();
        chain
    }

    fn flatten(&self, i: usize) -> Flat {
        let chain = self.chain_of(i);
        let mut f = Flat::default();
        let split = self.default_para.and_then(|d| chain.iter().position(|&c| c == d));
        for (pos, &c) in chain.iter().enumerate() {
            let s = &self.styles[c];
            f.all.overlay(&s.own);
            match split {
                Some(sp) if pos <= sp => f.default_prefix.get_or_insert_with(Layer::default).overlay(&s.own),
                _ => f.rest.overlay(&s.own),
            }
            for (t, l) in &s.conds {
                f.conds[*t as usize].get_or_insert_with(Layer::default).overlay(l);
            }
            f.row_band = s.row_band.or(f.row_band);
            f.col_band = s.col_band.or(f.col_band);
        }
        f.chain = chain;
        f
    }

    /// Table-style layer for a table key: whole-table properties, then each active
    /// conditional format in application order.
    fn table_layer(&self, key: TableKey) -> Option<Layer> {
        let (t, cond) = key?;
        let f = &self.flats[t];
        let mut l = f.all.clone();
        for c in cond.iter() {
            if let Some(cl) = &f.conds[c as usize] {
                l.overlay(cl);
            }
        }
        Some(l)
    }

    /// The numbering level formatting for (numId, ilvl).
    fn level(&self, num_id: i32, ilvl: i32) -> Option<&Level> {
        let mut num_id = num_id;
        for _ in 0..4 {
            let (abs, overrides) = self.numbering.nums.get(&num_id)?;
            if let Some(l) = overrides.get(&ilvl) {
                return Some(l);
            }
            let (levels, link) = self.numbering.abstracts.get(abs)?;
            if let Some(l) = levels.get(&ilvl) {
                return Some(l);
            }
            // numStyleLink → numbering style → its numId.
            let style = self.index_of_kind(link.as_deref(), StyleKind::Numbering)?;
            num_id = self.flats[style].all.ppr.num_id?;
        }
        None
    }

    // ------------------------------------------------------------------ cascades

    fn para_base(&self, key: ParaKey) -> Arc<PPr> {
        if let Some(hit) = lock(&self.para_cache).get(&key) {
            return hit.clone();
        }
        let (pidx, table, num) = key;
        let mut acc = self.defaults_ppr.clone();
        let flat = pidx.map(|i| &self.flats[i]);
        if let Some(prefix) = flat.and_then(|f| f.default_prefix.as_ref()) {
            acc.overlay(&prefix.ppr);
        }
        if let Some(t) = self.table_layer(table) {
            acc.overlay(&t.ppr);
        }
        let level = num.and_then(|(n, l, _)| self.level(n, l));
        let direct_num = num.is_some_and(|(_, _, d)| d);
        if let (Some(l), false) = (level, direct_num) {
            acc.overlay(&l.ppr);
        }
        if let Some(f) = flat {
            acc.overlay(&f.rest.ppr);
        }
        if let (Some(l), true) = (level, direct_num) {
            acc.overlay(&l.ppr);
        }
        let arc = Arc::new(acc);
        lock(&self.para_cache).insert(key, arc.clone());
        arc
    }

    fn run_base(&self, key: RunKey) -> Arc<RPr> {
        if let Some(hit) = lock(&self.run_cache).get(&key) {
            return hit.clone();
        }
        let (pidx, cidx, table) = key;
        let mut acc = self.defaults_rpr.clone();
        let pflat = pidx.map(|i| &self.flats[i]);
        let cflat = cidx.map(|i| &self.flats[i]);
        let tlayer = self.table_layer(table);
        if let Some(prefix) = pflat.and_then(|f| f.default_prefix.as_ref()) {
            acc.overlay_except_toggles(&prefix.rpr);
        }
        if let Some(t) = &tlayer {
            acc.overlay_except_toggles(&t.rpr);
        }
        if let Some(f) = pflat {
            acc.overlay_except_toggles(&f.rest.rpr);
        }
        if let Some(f) = cflat {
            acc.overlay_except_toggles(&f.all.rpr);
        }
        // Toggles: XOR the per-type chain results over docDefaults.
        let linked = match (pidx, cidx) {
            (Some(p), Some(c)) => {
                let (ps, cs) = (&self.styles[p], &self.styles[c]);
                ps.link.as_deref() == Some(cs.id.as_str()) || cs.link.as_deref() == Some(ps.id.as_str())
            }
            _ => false,
        };
        for i in 0..TOGGLES.len() {
            let mut v = self.defaults_rpr.toggles[i];
            let mut toggle = |t: Option<bool>| match t {
                Some(true) => v = Some(!v.unwrap_or(false)),
                Some(false) => v = Some(v.unwrap_or(false)),
                None => {}
            };
            toggle(tlayer.as_ref().and_then(|t| t.rpr.toggles[i]));
            toggle(pflat.and_then(|f| f.all.rpr.toggles[i]));
            let c = cflat.and_then(|f| f.all.rpr.toggles[i]);
            if linked {
                if c.is_some() {
                    v = c;
                }
            } else {
                toggle(c);
            }
            acc.toggles[i] = v;
        }
        let arc = Arc::new(acc);
        lock(&self.run_cache).insert(key, arc.clone());
        arc
    }

    // ------------------------------------------------------------------ public resolution

    /// Effective properties of paragraph `p` (a `w:p`). `ctx` says where it sits in a table
    /// ([`TableCtx::of`], or [`TableCtx::none`] outside tables).
    pub fn resolve_paragraph(&self, doc: &Document, p: NodeId, ctx: &TableCtx) -> EffectiveParagraph {
        let direct = doc.find_child(p, W, "pPr").map(|n| PPr::parse(doc, n)).unwrap_or_default();
        let pidx = self.para_index(direct.style.as_deref());
        let style_ppr = pidx.map(|i| &self.flats[i].all.ppr);
        let num_id = direct.num_id.or(style_ppr.and_then(|s| s.num_id));
        let ilvl = direct.ilvl.or(style_ppr.and_then(|s| s.ilvl)).unwrap_or(0).clamp(0, 8);
        let num = num_id.filter(|&n| n > 0).map(|n| (n, ilvl, direct.num_id.is_some()));
        let table = self.table_key(ctx);
        let base = self.para_base((pidx, table, num));
        let mut acc = (*base).clone();
        acc.overlay(&direct);
        let mut out = self.finish_paragraph(&acc);
        out.style = pidx.map(|i| self.styles[i].id.clone());
        out.numbering = num.map(|(num_id, ilvl, _)| NumberingRef { num_id, ilvl: ilvl as u8 });
        out.ctx = ParaCtx { style: pidx.map(|i| self.styles[i].id.clone().into_boxed_str()), table: ctx.clone() };
        out
    }

    /// [`StyleSheet::resolve_paragraph`] with the table context computed from the tree.
    pub fn resolve_paragraph_at(&self, doc: &Document, p: NodeId) -> EffectiveParagraph {
        let ctx = TableCtx::of(self, doc, p);
        self.resolve_paragraph(doc, p, &ctx)
    }

    /// Effective properties of run `r` (a `w:r`) in a paragraph resolved earlier.
    pub fn resolve_run(&self, doc: &Document, r: NodeId, para: &EffectiveParagraph) -> EffectiveRun {
        let direct = doc.find_child(r, W, "rPr").map(|n| RPr::parse(doc, n)).unwrap_or_default();
        let acc = self.run_props(&direct, &para.ctx);
        let hint = acc.hint.as_deref() == Some("eastAsia");
        let force_cs = acc.rtl == Some(true) || acc.cs == Some(true);
        let script = run_script(doc, r, hint, force_cs);
        self.finish_run(&acc, Some(script))
    }

    /// Effective properties of a run with no direct formatting in a paragraph resolved
    /// earlier: the paragraph's style baseline that direct run formatting is shown relative to.
    /// With `EffectiveParagraph::default()` this is the document defaults' run.
    pub fn resolve_style_run(&self, para: &EffectiveParagraph) -> EffectiveRun {
        let acc = self.run_props(&RPr::default(), &para.ctx);
        self.finish_run(&acc, None)
    }

    /// Effective properties of the paragraph mark (`w:pPr/w:rPr`): what an empty paragraph
    /// looks like and what new text typed at its end gets.
    pub fn resolve_paragraph_mark(&self, doc: &Document, p: NodeId, para: &EffectiveParagraph) -> EffectiveRun {
        let acc = self.mark_props(doc, p, para);
        self.finish_run(&acc, None)
    }

    /// Effective properties of a numbered paragraph's list label: the paragraph mark's
    /// properties with the numbering level's `w:rPr` on top. `None` when the paragraph is not
    /// numbered or the level is unknown (numbering not loaded).
    pub fn resolve_list_label(&self, doc: &Document, p: NodeId, para: &EffectiveParagraph) -> Option<EffectiveRun> {
        let n = para.numbering?;
        let level = self.level(n.num_id, i32::from(n.ilvl))?;
        let mut acc = self.mark_props(doc, p, para);
        acc.overlay(&level.rpr);
        Some(self.finish_run(&acc, None))
    }

    /// Effective cell properties of `tc` (a `w:tc`): table style whole-table and conditional
    /// `w:tcPr`, then the cell's own.
    pub fn resolve_cell(&self, doc: &Document, tc: NodeId, ctx: &TableCtx) -> EffectiveCell {
        let mut acc = self.table_layer(self.table_key(ctx)).map(|l| l.tcpr).unwrap_or_default();
        if let Some(n) = doc.find_child(tc, W, "tcPr") {
            acc.overlay(&TcPr::parse(doc, n));
        }
        let b = |i: usize| acc.borders[i].as_ref().map(|s| self.border(s));
        let borders = CellBorders {
            top: b(0),
            left: b(1),
            bottom: b(2),
            right: b(3),
            inside_h: b(4),
            inside_v: b(5),
            tl2br: b(6),
            tr2bl: b(7),
        };
        debug_assert_eq!(CELL_BORDERS.len(), 8);
        EffectiveCell {
            shading: acc.shd.as_ref().map(|s| self.shading(s)),
            v_align: acc.v_align.as_deref().map(str::to_owned),
            borders: (borders != CellBorders::default()).then_some(borders),
            no_wrap: acc.no_wrap,
            text_direction: acc.text_direction.as_deref().map(str::to_owned),
        }
    }

    fn run_props(&self, direct: &RPr, ctx: &ParaCtx) -> RPr {
        let pidx = self.para_index(ctx.style.as_deref());
        let cidx = self.index_of_kind(direct.style.as_deref(), StyleKind::Character);
        let base = self.run_base((pidx, cidx, self.table_key(&ctx.table)));
        let mut acc = (*base).clone();
        acc.overlay(direct);
        acc.style = cidx.map(|i| self.styles[i].id.clone().into_boxed_str());
        acc
    }

    fn mark_props(&self, doc: &Document, p: NodeId, para: &EffectiveParagraph) -> RPr {
        let direct = doc
            .find_child(p, W, "pPr")
            .and_then(|ppr| doc.find_child(ppr, W, "rPr"))
            .map(|n| RPr::parse(doc, n))
            .unwrap_or_default();
        self.run_props(&direct, &para.ctx)
    }

    // ------------------------------------------------------------------ finishing

    fn color(&self, c: &ColorSpec) -> Option<String> {
        if let (Some(tc), Some(theme)) = (c.theme, self.theme.as_ref())
            && let Some(mut rgb) = theme.color(tc, &self.settings.color_map)
        {
            if let Some(t) = c.tint {
                rgb = rgb.tint(t);
            }
            if let Some(s) = c.shade {
                rgb = rgb.shade(s);
            }
            return Some(rgb.hex());
        }
        match c.val {
            Some(Some(rgb)) => Some(rgb.hex()),
            Some(None) => Some("auto".to_owned()),
            None => None,
        }
    }

    fn shading(&self, s: &ShdSpec) -> Shading {
        Shading { val: s.val.as_deref().map(str::to_owned), color: self.color(&s.color), fill: self.color(&s.fill) }
    }

    fn border(&self, b: &BorderSpec) -> Border {
        Border {
            val: b.val.as_deref().map(str::to_owned),
            sz: b.sz,
            space: b.space,
            color: self.color(&b.color),
            shadow: b.shadow,
            frame: b.frame,
        }
    }

    fn finish_run(&self, r: &RPr, script: Option<Script>) -> EffectiveRun {
        let lang_ea = r.lang[1].as_deref().or(self.settings.theme_font_lang[1].as_deref());
        let lang_bidi = r.lang[2].as_deref().or(self.settings.theme_font_lang[2].as_deref());
        let font = |i: usize, lang: Option<&str>| -> Option<String> {
            let slot = r.fonts[i].as_ref()?;
            slot.theme.and_then(|t| self.theme.as_ref()?.font(t, lang)).or(slot.name.as_deref()).map(str::to_owned)
        };
        let fonts = Fonts {
            ascii: font(props::ASCII, None),
            h_ansi: font(props::HANSI, None),
            east_asia: font(props::EAST_ASIA, lang_ea),
            cs: font(props::CS, lang_bidi),
            hint: r.hint.as_deref().map(str::to_owned),
        };
        let script = script.unwrap_or_else(|| {
            if r.rtl == Some(true) || r.cs == Some(true) { Script::ComplexScript } else { Script::Ascii }
        });
        let chosen = fonts
            .for_script(script)
            .or(if script == Script::HAnsi { fonts.ascii.as_deref() } else { None })
            .map(str::to_owned);
        let t = |i: usize| r.toggles[i];
        let pos = |v: Option<i32>| v.and_then(|v| u32::try_from(v).ok());
        let s = |v: &Option<Box<str>>| v.as_deref().map(str::to_owned);
        let lang = Lang { val: s(&r.lang[0]), east_asia: s(&r.lang[1]), bidi: s(&r.lang[2]) };
        EffectiveRun {
            style: s(&r.style),
            fonts: (!fonts.is_empty()).then_some(fonts),
            font: chosen,
            script: Some(script),
            bold: t(0),
            bold_cs: t(1),
            italic: t(2),
            italic_cs: t(3),
            caps: t(4),
            small_caps: t(5),
            strike: t(6),
            dstrike: t(7),
            outline: t(8),
            shadow: t(9),
            emboss: t(10),
            imprint: t(11),
            vanish: t(12),
            size: pos(r.sz),
            size_cs: pos(r.sz_cs),
            color: r.color.as_ref().and_then(|c| self.color(c)),
            highlight: s(&r.highlight),
            shading: r.shd.as_ref().map(|x| self.shading(x)),
            underline: r.u.as_ref().map(|u| Underline { val: s(&u.val), color: self.color(&u.color) }),
            vert_align: s(&r.vert_align),
            position: r.position,
            kern: pos(r.kern),
            spacing: r.spacing,
            scale: pos(r.w),
            lang: (lang != Lang::default()).then_some(lang),
            rtl: r.rtl,
            cs: r.cs,
            spec_vanish: r.spec_vanish,
            web_hidden: r.web_hidden,
            no_proof: r.no_proof,
            snap_to_grid: r.snap_to_grid,
            border: r.bdr.as_ref().map(|b| self.border(b)),
            emphasis_mark: s(&r.em),
            effect: s(&r.effect),
        }
    }

    fn finish_paragraph(&self, p: &PPr) -> EffectiveParagraph {
        let jc = p.jc.as_deref().map(|j| {
            match j {
                "left" => "start",
                "right" => "end",
                other => other,
            }
            .to_owned()
        });
        let i = &p.ind;
        let indent = Indent {
            start: i.start,
            end: i.end,
            hanging: i.hanging,
            first_line: i.first_line,
            start_chars: i.start_chars,
            end_chars: i.end_chars,
            hanging_chars: i.hanging_chars,
            first_line_chars: i.first_line_chars,
        };
        let sp = &p.spacing;
        let spacing = Spacing {
            before: sp.before,
            after: sp.after,
            line: sp.line,
            line_rule: sp.line_rule.as_deref().map(str::to_owned).or_else(|| sp.line.map(|_| "auto".to_owned())),
            before_autospacing: sp.before_autospacing,
            after_autospacing: sp.after_autospacing,
            before_lines: sp.before_lines,
            after_lines: sp.after_lines,
        };
        let b = |i: usize| p.borders[i].as_ref().map(|s| self.border(s));
        debug_assert_eq!(PARA_BORDERS.len(), 6);
        let borders = ParagraphBorders { top: b(0), left: b(1), bottom: b(2), right: b(3), between: b(4), bar: b(5) };
        let f = |i: usize| p.flags[i];
        EffectiveParagraph {
            style: None,
            justification: jc,
            indent: (indent != Indent::default()).then_some(indent),
            spacing: (spacing != Spacing::default()).then_some(spacing),
            keep_next: f(0),
            keep_lines: f(1),
            page_break_before: f(2),
            widow_control: f(3),
            contextual_spacing: f(4),
            bidi: f(5),
            suppress_auto_hyphens: f(6),
            suppress_line_numbers: f(7),
            mirror_indents: f(8),
            snap_to_grid: f(9),
            word_wrap: f(10),
            suppress_overlap: f(11),
            outline_level: p.outline_lvl.map(|v| v.clamp(0, 9) as u8),
            borders: (borders != ParagraphBorders::default()).then_some(borders),
            shading: p.shd.as_ref().map(|s| self.shading(s)),
            tabs: p
                .final_tabs()
                .into_iter()
                .map(|t| TabStop { val: t.val.to_string(), pos: t.pos, leader: t.leader.as_deref().map(str::to_owned) })
                .collect(),
            numbering: None,
            text_alignment: p.text_alignment.as_deref().map(str::to_owned),
            ctx: ParaCtx::default(),
        }
    }
}

/// The script of a run's text, scanning only until the first character that decides it.
fn run_script(doc: &Document, r: NodeId, hint: bool, force_cs: bool) -> Script {
    if force_cs {
        return Script::ComplexScript;
    }
    let mut first: Option<char> = None;
    for (c, name) in w_children(doc, r) {
        if !matches!(name, "t" | "delText") {
            continue;
        }
        for t in doc.children(c) {
            let Some(text) = doc.text(t) else { continue };
            for ch in text.chars() {
                first.get_or_insert(ch);
                if !(ch.is_whitespace() || ch.is_ascii_digit() || ch.is_ascii_punctuation() || ch.is_control()) {
                    return script_of(ch, hint, false);
                }
            }
        }
    }
    first.map_or(Script::Ascii, |c| script_of(c, hint, false))
}

/// Canonical (`w:name`) name for a localised built-in style name, lower case.
fn builtin_name(q: &str) -> Option<String> {
    const HEADING_PREFIXES: [&str; 8] =
        ["kop ", "überschrift ", "uberschrift ", "titre ", "título ", "titolo ", "rubrik ", "nagłówek "];
    for p in HEADING_PREFIXES {
        if let Some(n) = q.strip_prefix(p)
            && n.len() == 1
            && n.as_bytes()[0].is_ascii_digit()
            && n != "0"
        {
            return Some(format!("heading {n}"));
        }
    }
    const TOC_PREFIXES: [&str; 4] = ["inhopg ", "verzeichnis ", "tm ", "tdc "];
    for p in TOC_PREFIXES {
        if let Some(n) = q.strip_prefix(p)
            && n.len() == 1
            && n.as_bytes()[0].is_ascii_digit()
            && n != "0"
        {
            return Some(format!("toc {n}"));
        }
    }
    const NAMES: &[(&str, &str)] = &[
        ("standaard", "normal"),
        ("standard", "normal"),
        ("normale", "normal"),
        ("titel", "title"),
        ("titre", "title"),
        ("título", "title"),
        ("ondertitel", "subtitle"),
        ("untertitel", "subtitle"),
        ("sous-titre", "subtitle"),
        ("lijstalinea", "list paragraph"),
        ("listenabsatz", "list paragraph"),
        ("paragraphe de liste", "list paragraph"),
        ("párrafo de lista", "list paragraph"),
        ("geen afstand", "no spacing"),
        ("kein leerraum", "no spacing"),
        ("sans interligne", "no spacing"),
        ("sin espaciado", "no spacing"),
        ("citaat", "quote"),
        ("zitat", "quote"),
        ("citation", "quote"),
        ("duidelijk citaat", "intense quote"),
        ("tabelraster", "table grid"),
        ("tabellenraster", "table grid"),
        ("grille du tableau", "table grid"),
        ("koptekst", "header"),
        ("kopfzeile", "header"),
        ("en-tête", "header"),
        ("voettekst", "footer"),
        ("fußzeile", "footer"),
        ("pied de page", "footer"),
        ("voetnoottekst", "footnote text"),
        ("funotentext", "footnote text"),
        ("note de bas de page", "footnote text"),
        ("plattetekst", "body text"),
        ("textkörper", "body text"),
        ("corps de texte", "body text"),
        ("standaardalinea-lettertype", "default paragraph font"),
        ("absatz-standardschriftart", "default paragraph font"),
        ("police par défaut", "default paragraph font"),
        ("nadruk", "emphasis"),
        ("hervorhebung", "emphasis"),
        ("accentuation", "emphasis"),
        ("zwaar", "strong"),
        ("fett", "strong"),
        ("lijstopsommingteken", "list bullet"),
        ("aufzählungszeichen", "list bullet"),
        ("lijstnummer", "list number"),
        ("listennummer", "list number"),
        ("bijschrift", "caption"),
        ("beschriftung", "caption"),
        ("légende", "caption"),
        ("ballontekst", "balloon text"),
        ("sprechblasentext", "balloon text"),
        ("tekst opmerking", "annotation text"),
        ("kommentartext", "annotation text"),
        ("kopvaninhoudsopgave", "toc heading"),
        ("kop van inhoudsopgave", "toc heading"),
        ("inhaltsverzeichnisüberschrift", "toc heading"),
    ];
    NAMES.iter().find(|(k, _)| *k == q).map(|(_, v)| (*v).to_owned())
}
