//! The viewer's view model (`spec/schema/view-model.schema.json`): one resolved, render-ready
//! JSON tree of a document version. Built in markup view: every tracked change is present with
//! its revision, so the viewer can show or hide them itself.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::anchors::Anchors;
use crate::wml::style::{Border, Script, StyleSheet, TableCtx};
use crate::wml::tree::{child_on, child_val, w_attr};
use crate::wml::view::{self, Block as VBlock, Paragraph as VParagraph, Table as VTable, VMerge};
use crate::wml::{HfType, Loc, PartIdx, StoryKind, WordDoc};
use crate::xml::{Document, NodeId, ns};

use super::assets::{self, Asset};
use super::ctx::{self, DocCtx, DocIndex};
use super::image;
use super::inline::{self, Atom, Control, InlineState, Link, Marks, Piece};
use super::render::{self, Renderer};
use super::{ChangeSetInfo, ProjectOptions, SPEC_VERSION, View};

/// Options of [`view_model`]: the same session facts the projection takes.
pub type ViewModelOptions = ProjectOptions;

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------------------------- shapes

/// The whole view model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewModel {
    pub spec_version: String,
    pub version: u64,
    pub meta: Meta,
    pub sections: Vec<Section>,
    /// The body story, in document order.
    pub blocks: Vec<Block>,
    /// Headers, footers, notes and text boxes, keyed by story id.
    pub stories: BTreeMap<String, Story>,
    pub comments: Vec<Comment>,
    #[serde(rename = "changeSets")]
    pub change_sets: Vec<ChangeSetInfo>,
    pub assets: BTreeMap<String, VmAsset>,
}

impl ViewModel {
    /// The blocks of a story (`b`, `h1`, …, or a comment id `c1`).
    pub fn story_blocks(&self, story: &str) -> Option<&Vec<Block>> {
        if story == "b" {
            return Some(&self.blocks);
        }
        if let Some(s) = self.stories.get(story) {
            return Some(&s.blocks);
        }
        self.comments.iter().find(|c| c.id == story).map(|c| &c.blocks)
    }

    pub(crate) fn story_blocks_mut(&mut self, story: &str) -> Option<&mut Vec<Block>> {
        if story == "b" {
            return Some(&mut self.blocks);
        }
        if let Some(s) = self.stories.get_mut(story) {
            return Some(&mut s.blocks);
        }
        self.comments.iter_mut().find(|c| c.id == story).map(|c| &mut c.blocks)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub title: Option<String>,
    pub lang: String,
    pub track_changes: bool,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages_estimate: Option<u64>,
    #[serde(default)]
    pub read_only: Option<String>,
    pub flags: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fonts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: String,
    pub page: Page,
    pub headers: HfRefs,
    pub footers: HfRefs,
    pub title_page: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub w: i64,
    pub h: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orient: Option<String>,
    pub margins: Margins,
    pub cols: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Margins {
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
    pub left: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gutter: Option<i64>,
}

/// Header or footer story per slot (inherited from earlier sections, as Word does).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HfRefs {
    pub default: Option<String>,
    pub first: Option<String>,
    pub even: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Story {
    pub kind: String,
    pub blocks: Vec<Block>,
}

/// A block: paragraph, table or image block.
#[allow(clippy::large_enum_variant)] // built once per block, never moved around in bulk
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Block {
    #[serde(rename = "p")]
    P(Paragraph),
    #[serde(rename = "table")]
    Table(Table),
    #[serde(rename = "image")]
    Image(ImageBlock),
}

impl Block {
    pub fn id(&self) -> &str {
        match self {
            Block::P(p) => &p.id,
            Block::Table(t) => &t.id,
            Block::Image(i) => &i.image.id,
        }
    }

    /// Every anchor inside this block: itself, a table's rows, cells and nested blocks, and
    /// inline images.
    pub fn anchors(&self, out: &mut Vec<String>) {
        out.push(self.id().to_owned());
        match self {
            Block::P(p) => {
                for i in &p.inlines {
                    if let Some(img) = &i.image {
                        out.push(img.id.clone());
                    }
                }
            }
            Block::Image(i) => out.push(i.para.clone()),
            Block::Table(t) => {
                for r in &t.rows {
                    out.push(r.id.clone());
                    for c in &r.cells {
                        out.push(c.id.clone());
                        for b in &c.blocks {
                            b.anchors(out);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paragraph {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub style: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<ListInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt: Option<ParagraphFormat>,
    pub inlines: Vec<Inline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Revision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppr_change: Option<Revision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ContentControl>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub section_end: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListInfo {
    pub label: String,
    pub level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_id: Option<i32>,
}

/// Resolved paragraph formatting (style + direct), including the paragraph style's run
/// formatting (font, size, color, bold, italic, caps) that inline `marks` are relative to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphFormat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bidi: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent: Option<Indent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Spacing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub b: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub i: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub caps: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_next: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub page_break_before: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Indent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hanging: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spacing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_rule: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub id: u64,
    #[serde(rename = "type")]
    pub typ: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default)]
    pub cs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmLink {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmField {
    #[serde(rename = "type")]
    pub typ: String,
    pub instr: String,
    pub editable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentControl {
    pub tag: Option<String>,
    pub alias: Option<String>,
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bound: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

impl ContentControl {
    fn of(c: &Control) -> ContentControl {
        ContentControl {
            tag: c.tag.clone(),
            alias: c.alias.clone(),
            typ: c.typ.to_owned(),
            bound: c.bound,
            locked: c.locked,
        }
    }
}

/// One inline. `k` is `text`, `tab`, `br`, `field`, `image` or `note`; the other fields are
/// present as the schema allows for that kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inline {
    /// Defaults to `text` on input (the schema lets a text inline omit `k`); always written.
    #[serde(default = "text_kind")]
    pub k: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    #[serde(default, skip_serializing_if = "Marks::is_empty")]
    pub marks: Marks,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub br: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<VmField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Box<ImageBase>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Revision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmt_change: Option<Revision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<VmLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ContentControl>,
}

fn text_kind() -> String {
    "text".to_owned()
}

impl Inline {
    /// Text of this inline in the text model (accepted text: `\t` per tab, `\n` per line
    /// break, a field's result, nothing for deletions, images and note marks).
    pub fn accepted_text(&self) -> &str {
        if self.rev.as_ref().is_some_and(|r| r.typ == "del" || r.typ == "moveFrom") {
            return "";
        }
        match self.k.as_str() {
            "text" | "field" => self.t.as_deref().unwrap_or(""),
            "tab" => "\t",
            "br" => "\n",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crop {
    pub l: i64,
    pub t: i64,
    pub r: i64,
    pub b: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageBase {
    pub id: String,
    pub drawing: String,
    pub asset: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
    pub w: i64,
    pub h: i64,
    pub wrap: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    pub alt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub decorative: bool,
    pub crop: Option<Crop>,
    pub rotation: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_h: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_v: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// The text-box story this drawing hosts (`tb1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub textbox: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageBlock {
    #[serde(flatten)]
    pub image: ImageBase,
    pub para: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Revision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub grid: Vec<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bidi: bool,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub header: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Revision>,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorderLine {
    /// `w:val` (`single`, `double`, `dashed`, …).
    pub style: String,
    /// Eighths of a point.
    pub size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Resolved cell borders: cell, then table style, then the table's own `w:tblBorders`
/// (outer edges for outer cells, `insideH`/`insideV` between cells). `null` = no line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellBorders {
    pub top: Option<BorderLine>,
    pub right: Option<BorderLine>,
    pub bottom: Option<BorderLine>,
    pub left: Option<BorderLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: String,
    pub span: u32,
    pub vmerge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borders: Option<CellBorders>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentAnchor {
    pub start: String,
    pub end: String,
    pub text: String,
    /// UTF-16 offset into `start`'s text (text model), where the range begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_offset: Option<usize>,
    /// UTF-16 offset into `end`'s text where the range ends (exclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub parent: Option<String>,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initials: Option<String>,
    pub date: Option<String>,
    pub done: bool,
    pub agent: bool,
    pub anchor: Option<CommentAnchor>,
    pub blocks: Vec<Block>,
}

/// A view-model `assets` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmAsset {
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,
    pub px: [u32; 2],
    pub bytes: u64,
}

// ---------------------------------------------------------------------------------- build

/// Build the view model of `doc`.
pub fn view_model(doc: &WordDoc, anchors: &Anchors, sheet: &StyleSheet, opts: &ViewModelOptions) -> Result<ViewModel> {
    view_model_with(doc, anchors, sheet, None, opts)
}

pub(crate) fn view_model_with(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    opts: &ViewModelOptions,
) -> Result<ViewModel> {
    let cx = DocCtx::with(doc, anchors, sheet, opts, View::Markup, idx);
    let mut b = Builder {
        cx: &cx,
        r: Renderer::new(&cx),
        by_part: assets::by_part(doc),
        assets: BTreeMap::new(),
        spans: HashMap::new(),
        points: HashMap::new(),
        track_spans: true,
        fonts: BTreeSet::new(),
        body_chars: 0,
        first_heading: None,
        cur_section: 0,
        border_cache: HashMap::new(),
    };
    let body = b.story_blocks(0, true);
    let mut stories = BTreeMap::new();
    for (si, st) in doc.stories().iter().enumerate() {
        let kind = match &st.kind {
            StoryKind::Header { .. } => "header",
            StoryKind::Footer { .. } => "footer",
            StoryKind::Footnote { .. } => "footnote",
            StoryKind::Endnote { .. } => "endnote",
            StoryKind::TextBox { .. } => "textbox",
            StoryKind::Body | StoryKind::Comment { .. } => continue,
        };
        let id = cx.story_id(si);
        if !valid_story_key(&id) || stories.contains_key(&id) {
            continue;
        }
        let blocks = b.story_blocks(si, false);
        stories.insert(id, Story { kind: kind.into(), blocks });
    }
    b.track_spans = false;
    let comments = b.comments();
    let sections = b.sections();
    let meta = b.meta();
    Ok(ViewModel {
        spec_version: SPEC_VERSION.into(),
        version: opts.version_of(doc),
        meta,
        sections,
        blocks: body,
        stories,
        comments,
        change_sets: opts.change_sets.clone(),
        assets: b.assets,
    })
}

/// The view-model blocks of single paragraphs, each built on its own: no list, field or comment
/// state is carried in from the paragraphs before it. Op-result lint reads the paragraphs a
/// transaction changed this way instead of building the whole view model.
pub(crate) fn para_blocks(
    doc: &WordDoc,
    anchors: &Anchors,
    sheet: &StyleSheet,
    idx: Option<&DocIndex>,
    opts: &ViewModelOptions,
    paras: &[Loc],
) -> Vec<Block> {
    let cx = DocCtx::with(doc, anchors, sheet, opts, View::Markup, idx);
    let mut b = Builder {
        cx: &cx,
        r: Renderer::new(&cx),
        by_part: assets::by_part(doc),
        assets: BTreeMap::new(),
        spans: HashMap::new(),
        points: HashMap::new(),
        track_spans: false,
        fonts: BTreeSet::new(),
        body_chars: 0,
        first_heading: None,
        cur_section: 0,
        border_cache: HashMap::new(),
    };
    paras.iter().filter_map(|l| b.para(l.part, l.node, &mut InlineState::default(), None, false)).collect()
}

fn valid_story_key(id: &str) -> bool {
    let digits = id.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let prefix = &id[..id.len() - digits.len()];
    matches!(prefix, "h" | "f" | "fn" | "en" | "tb")
        && !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// A comment range seen while walking: where it starts and ends, and the text it covers.
#[derive(Debug, Default)]
struct Span {
    start: Option<(String, usize)>,
    end: Option<(String, usize)>,
    text: String,
    last: Option<String>,
}

type TblEdges = [Option<BorderLine>; 6];

struct Builder<'c, 'a> {
    cx: &'c DocCtx<'a>,
    r: Renderer<'c, 'a>,
    by_part: BTreeMap<String, Asset>,
    assets: BTreeMap<String, VmAsset>,
    spans: HashMap<i64, Span>,
    points: HashMap<i64, (String, usize)>,
    track_spans: bool,
    fonts: BTreeSet<String>,
    body_chars: usize,
    first_heading: Option<String>,
    cur_section: usize,
    border_cache: HashMap<(PartIdx, NodeId), TblEdges>,
}

impl Builder<'_, '_> {
    fn story_blocks(&mut self, si: usize, body: bool) -> Vec<Block> {
        let story = self.cx.story(si);
        let mut st = InlineState::default();
        let mut out = Vec::new();
        self.blocks(story.part, story.root, &mut st, body, None, &mut out);
        out
    }

    /// Blocks under `root`, with block-level content controls flattened onto their paragraphs.
    fn blocks(
        &mut self,
        part: PartIdx,
        root: NodeId,
        st: &mut InlineState,
        body: bool,
        control: Option<&ContentControl>,
        out: &mut Vec<Block>,
    ) {
        let d = self.cx.doc.part(part);
        for blk in view::blocks(d, root) {
            match blk {
                VBlock::Paragraph(p) => {
                    let section = body.then(|| format!("s{}", self.cur_section + 1));
                    let ends = if body { self.section_end(part, p.0) } else { None };
                    if let Some(mut b) = self.para(part, p.0, st, control, body) {
                        match &mut b {
                            Block::P(x) => {
                                x.section = section;
                                x.section_end = ends.is_some();
                            }
                            Block::Image(x) => x.section = section,
                            Block::Table(_) => {}
                        }
                        out.push(b);
                    }
                    if let Some(next) = ends {
                        self.cur_section = next;
                    }
                }
                VBlock::Table(t) => {
                    if let Some(mut tb) = self.table(part, t, st, body) {
                        if body {
                            tb.section = Some(format!("s{}", self.cur_section + 1));
                        }
                        out.push(Block::Table(tb));
                    }
                }
                VBlock::Sdt(s) => {
                    let c = Control::of(d, s.0);
                    let cc = ContentControl::of(&c);
                    let inner = s.content(d);
                    if let Some(inner) = inner {
                        self.blocks(part, inner, st, body, Some(&cc), out);
                    }
                }
            }
        }
    }

    /// The index of the next section, when paragraph `p` ends a non-final section.
    fn section_end(&self, part: PartIdx, p: NodeId) -> Option<usize> {
        let d = self.cx.doc.part(part);
        let ppr = VParagraph(p).ppr(d)?;
        let sp = d.find_child(ppr, ns::W, "sectPr")?;
        let secs = self.cx.doc.sections();
        let k = secs.iter().position(|s| s.sect_pr == sp)?;
        (k + 1 < secs.len()).then_some(k + 1)
    }

    // ------------------------------------------------------------------------- paragraphs

    fn para(
        &mut self,
        part: PartIdx,
        p: NodeId,
        st: &mut InlineState,
        control: Option<&ContentControl>,
        body: bool,
    ) -> Option<Block> {
        let cx = self.cx;
        let d = cx.doc.part(part);
        let pieces = inline::pieces(cx, part, p, st);
        let anchor = cx.anchors.handle_of(Loc::new(part, p))?.to_owned();
        if self.track_spans {
            self.track(&anchor, &pieces);
        }
        let plain_len: usize = pieces.iter().map(|x| x.plain.chars().count()).sum();
        if body {
            self.body_chars += plain_len;
        }
        let heading = self.r.heading_level(part, p);
        if body && heading.is_some() && self.first_heading.is_none() {
            let t: String = pieces.iter().map(|x| x.plain.as_str()).collect();
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                self.first_heading = Some(t);
            }
        }
        // A paragraph that holds only one drawing: an image block.
        let mut img = None;
        let image_only = pieces.iter().all(|x| match &x.atom {
            Atom::Image(n) => {
                let first = img.is_none();
                img = Some(*n);
                first && x.rev.is_none() && x.fence.is_none()
            }
            Atom::Text(t) => t.trim().is_empty() && x.rev.is_none(),
            _ => false,
        });
        if image_only
            && let Some(n) = img
            && let Some(image) = self.image(part, n)
        {
            return Some(Block::Image(ImageBlock { image, para: anchor, section: None, rev: None }));
        }
        let ep = cx.para(part, p);
        let style = ep
            .style
            .clone()
            .or_else(|| cx.sheet.default_paragraph_style_id().map(str::to_owned))
            .unwrap_or_else(|| "Normal".into());
        let style_name = cx.sheet.style_by_id(&style).and_then(|s| s.name()).map(str::to_owned);
        let list = self.r.label_parts(part, p).map(|(label, level)| ListInfo {
            label,
            level: level.min(8) as u8,
            num_id: ep.numbering.as_ref().map(|n| n.num_id),
        });
        let fmt = self.para_fmt(part, p);
        let inlines = self.inlines(part, &pieces);
        let mark = inline::mark_rev(d, p).map(|r| self.revision(r.kind.vm(), r.id, &r.author, r.date.as_deref(), None));
        let ppr_change = VParagraph(p)
            .ppr(d)
            .and_then(|ppr| d.find_child(ppr, ns::W, "pPrChange").map(|c| (ppr, c)))
            .map(|(ppr, c)| {
                let id = w_attr(d, c, "id").and_then(|v| v.trim().parse::<i64>().ok());
                let author = w_attr(d, c, "author").unwrap_or_default();
                let date = w_attr(d, c, "date");
                let new_style = child_val(d, ppr, "pStyle");
                let old_style = d.find_child(c, ns::W, "pPr").and_then(|op| child_val(d, op, "pStyle"));
                let old = (old_style != new_style).then(|| {
                    format!(
                        "style {}→{}",
                        old_style.as_deref().unwrap_or("Normal"),
                        new_style.as_deref().unwrap_or("Normal")
                    )
                });
                self.revision("pfmt", id, &author, date.as_deref(), old)
            });
        Some(Block::P(Paragraph {
            id: anchor,
            section: None,
            style,
            style_name,
            heading,
            list,
            fmt,
            inlines,
            mark,
            ppr_change,
            control: control.cloned(),
            section_end: false,
        }))
    }

    /// Record comment ranges covered by this paragraph's pieces.
    fn track(&mut self, anchor: &str, pieces: &[Piece]) {
        let mut off = 0usize;
        for x in pieces {
            let len = x.plain.encode_utf16().count();
            for &wid in x.comments.iter() {
                let sp = self.spans.entry(wid).or_default();
                if sp.start.is_none() {
                    sp.start = Some((anchor.to_owned(), off));
                }
                sp.end = Some((anchor.to_owned(), off + len));
                if !x.plain.is_empty() {
                    if sp.last.as_deref() != Some(anchor) {
                        if sp.last.is_some() {
                            sp.text.push('\n');
                        }
                        sp.last = Some(anchor.to_owned());
                    }
                    sp.text.push_str(&x.plain);
                }
            }
            if let Atom::Comment(wid) = x.atom {
                self.points.entry(wid).or_insert_with(|| (anchor.to_owned(), off));
            }
            off += len;
        }
    }

    fn para_fmt(&mut self, part: PartIdx, p: NodeId) -> Option<ParagraphFormat> {
        let cx = self.cx;
        let ep = cx.para(part, p);
        let br = cx.base_run(part, p);
        let s = Script::Ascii;
        let align = ep.justification.as_deref().and_then(|j| match j {
            "center" => Some("center"),
            "end" | "right" => Some("right"),
            "both" | "justify" | "lowKashida" | "mediumKashida" | "highKashida" => Some("both"),
            "distribute" | "thaiDistribute" => Some("distribute"),
            _ => None,
        });
        let indent = ep.indent.as_ref().map(|i| Indent {
            left: i.start.filter(|&v| v != 0),
            right: i.end.filter(|&v| v != 0),
            first_line: i.first_line.filter(|&v| v != 0),
            hanging: i.hanging.filter(|&v| v != 0),
        });
        let indent = indent.filter(|i| *i != Indent::default());
        let spacing = ep.spacing.as_ref().map(|sp| Spacing {
            before: sp.before,
            after: sp.after,
            line: sp.line,
            line_rule: sp.line_rule.as_deref().and_then(|r| match r {
                "auto" | "exact" | "atLeast" => Some(r.to_owned()),
                _ => None,
            }),
        });
        let spacing = spacing.filter(|s| *s != Spacing::default());
        let font = br.font_for(s).map(str::to_owned);
        if let Some(f) = &font {
            self.fonts.insert(f.clone());
        }
        let f = ParagraphFormat {
            align: align.map(str::to_owned),
            bidi: ep.bidi.unwrap_or(false),
            indent,
            spacing,
            font,
            size: br.size_for(s).filter(|&z| z >= 1),
            color: hex_color(br.color.as_deref()),
            shade: ep.shading.as_ref().and_then(|sh| hex_color(sh.fill.as_deref())),
            b: br.bold_for(s),
            i: br.italic_for(s),
            caps: br.caps.unwrap_or(false),
            keep_next: ep.keep_next.unwrap_or(false),
            page_break_before: ep.page_break_before.unwrap_or(false),
        };
        (f != ParagraphFormat::default()).then_some(f)
    }

    fn revision(&self, typ: &str, id: Option<i64>, author: &str, date: Option<&str>, old: Option<String>) -> Revision {
        Revision {
            id: id.unwrap_or(0).max(0) as u64,
            typ: typ.into(),
            author: author.into(),
            date: date.and_then(norm_date),
            cs: id.and_then(|i| self.cx.change_set(i)).map(|c| c.id.clone()),
            old,
        }
    }

    fn inlines(&mut self, part: PartIdx, pieces: &[Piece]) -> Vec<Inline> {
        let cx = self.cx;
        let mut out: Vec<Inline> = Vec::with_capacity(pieces.len());
        for x in pieces {
            let mut il = Inline {
                rev: x.rev.as_ref().map(|r| self.revision(r.kind.vm(), r.id, &r.author, r.date.as_deref(), None)),
                fmt_change: x
                    .fmt
                    .as_ref()
                    .map(|f| self.revision("fmt", f.id, &f.author, f.date.as_deref(), Some(f.desc.clone()))),
                link: x.link.as_deref().and_then(vm_link),
                ..Default::default()
            };
            let mut seen = BTreeSet::new();
            for wid in x.comments.iter() {
                if let Some(c) = cx.comment(*wid)
                    && seen.insert(c.sid.clone())
                {
                    il.comments.push(c.sid.clone());
                }
            }
            if let Some(f) = x.fence {
                il.flags.push(f.to_owned());
            }
            if x.zw {
                il.flags.push("zw".into());
            }
            let with_marks = |il: &mut Inline| {
                il.marks = (*x.marks).clone();
                il.style = x.style.as_deref().map(str::to_owned);
            };
            match &x.atom {
                Atom::Text(t) => {
                    if t.is_empty() {
                        continue;
                    }
                    il.k = "text".into();
                    il.t = Some(t.clone());
                    with_marks(&mut il);
                }
                Atom::Sym(u) => {
                    let Some(c) = char::from_u32(*u) else { continue };
                    il.k = "text".into();
                    il.t = Some(c.to_string());
                    with_marks(&mut il);
                }
                Atom::Tab => il.k = "tab".into(),
                Atom::Br(b) => {
                    il.k = "br".into();
                    il.br = Some(b.as_str().into());
                }
                Atom::Field(f) => {
                    let typ = f.typ();
                    il.k = "field".into();
                    il.t = Some(f.result.clone());
                    il.marks = (*x.marks).clone();
                    if il.link.is_none() {
                        il.link = f.link.as_ref().and_then(vm_link);
                    }
                    il.field = Some(VmField {
                        editable: matches!(typ.as_str(), "MERGEFIELD" | "REF" | "HYPERLINK"),
                        typ,
                        instr: f.instr.trim().to_owned(),
                        dirty: f.dirty,
                    });
                }
                Atom::Control(c) => {
                    let t = if c.display.is_empty() { x.plain.clone() } else { c.display.clone() };
                    if t.is_empty() {
                        continue;
                    }
                    il.k = "text".into();
                    il.t = Some(t);
                    with_marks(&mut il);
                    il.control = Some(ContentControl::of(c));
                }
                Atom::Image(n) => {
                    let Some(img) = self.image(part, *n) else { continue };
                    il.k = "image".into();
                    il.image = Some(Box::new(img));
                }
                Atom::Note { endnote, id } => {
                    let Some(sid) = cx.note(*endnote, *id) else { continue };
                    il.k = "note".into();
                    il.label = Some(sid.trim_start_matches(|c: char| c.is_ascii_alphabetic()).to_owned());
                    il.note = Some(sid.to_owned());
                }
                Atom::Comment(_) | Atom::ParaMark => continue,
            }
            if let Some(f) = &il.marks.font {
                self.fonts.insert(f.clone());
            }
            // Merge adjacent text inlines that differ only in their text.
            if il.k == "text"
                && il.control.is_none()
                && let Some(prev) = out.last_mut()
                && prev.k == "text"
                && prev.control.is_none()
            {
                let mut a = prev.clone();
                a.t = None;
                let mut b = il.clone();
                b.t = None;
                if a == b {
                    if let (Some(pt), Some(t)) = (prev.t.as_mut(), il.t.as_ref()) {
                        pt.push_str(t);
                    }
                    continue;
                }
            }
            out.push(il);
        }
        out
    }

    fn image(&mut self, part: PartIdx, n: NodeId) -> Option<ImageBase> {
        let cx = self.cx;
        let id = cx.anchors.handle_of(Loc::new(part, n))?.to_owned();
        let info = image::info(cx, part, n);
        let asset = info.media.as_ref().and_then(|m| self.by_part.get(m)).map(|a| {
            let e = assets::vm_entry(a, Some((info.w, info.h)));
            self.assets.insert(a.id.clone(), e);
            a.id.clone()
        });
        Some(ImageBase {
            id,
            drawing: info.drawing.into(),
            asset,
            external: info.external,
            w: info.w.max(0),
            h: info.h.max(0),
            wrap: info.wrap.into(),
            align: info.align.map(str::to_owned),
            alt: info.alt,
            title: info.title,
            decorative: info.decorative,
            crop: info.crop.map(|[l, t, r, b]| Crop { l, t, r, b }),
            rotation: info.rotation,
            flip_h: info.flip_h,
            flip_v: info.flip_v,
            caption: None,
            textbox: cx.textboxes.get(&Loc::new(part, n)).cloned(),
        })
    }

    // ----------------------------------------------------------------------------- tables

    fn table(&mut self, part: PartIdx, t: VTable, st: &mut InlineState, body: bool) -> Option<Table> {
        let cx = self.cx;
        let d = cx.doc.part(part);
        let ta = cx.anchors.handle_of(Loc::new(part, t.0))?.to_owned();
        let tbl_pr = d.find_child(t.0, ns::W, "tblPr");
        let grid = d
            .find_child(t.0, ns::W, "tblGrid")
            .map(|g| {
                d.child_elements(g)
                    .filter(|&c| d.is(c, ns::W, "gridCol"))
                    .map(|c| w_attr(d, c, "w").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0) as i64)
                    .collect()
            })
            .unwrap_or_default();
        let edges = self.tbl_edges(part, t.0);
        let rows = t.rows(d);
        let nrows = rows.len();
        let mut out_rows = Vec::with_capacity(nrows);
        for (ri, row) in rows.iter().enumerate() {
            let rid = format!("{ta}.r{}", ri + 1);
            let tr_pr = d.find_child(row.0, ns::W, "trPr");
            let rev = row.revision(d).and_then(|rv| {
                let typ = match rv.kind {
                    view::RevisionKind::RowIns => "rowIns",
                    view::RevisionKind::RowDel => "rowDel",
                    _ => return None,
                };
                Some(self.revision(typ, rv.id(d), &rv.author(d).unwrap_or_default(), rv.date(d).as_deref(), None))
            });
            let cells = row.cells(d);
            let ncells = cells.len();
            let mut out_cells = Vec::with_capacity(ncells);
            for (ci, c) in cells.iter().enumerate() {
                let mut blocks = Vec::new();
                self.blocks(part, c.0, st, false, None, &mut blocks);
                let _ = body;
                if blocks.is_empty() {
                    continue;
                }
                let ec = cx.sheet.resolve_cell(d, c.0, &TableCtx::of(cx.sheet, d, c.0));
                let tc_pr = d.find_child(c.0, ns::W, "tcPr");
                let width = tc_pr.and_then(|p| d.find_child(p, ns::W, "tcW")).and_then(|w| {
                    let typ = w_attr(d, w, "type");
                    if !matches!(typ.as_deref(), None | Some("dxa")) {
                        return None;
                    }
                    w_attr(d, w, "w").and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v as i64)
                });
                let (first_r, last_r, first_c, last_c) = (ri == 0, ri + 1 == nrows, ci == 0, ci + 1 == ncells);
                let cb = ec.borders.as_ref();
                let side = |own: Option<&Border>, outer: usize, inner: usize, is_outer: bool| -> Option<BorderLine> {
                    match own {
                        Some(b) => border_line(b.val.as_deref(), b.sz, b.color.as_deref()),
                        None => edges[if is_outer { outer } else { inner }].clone(),
                    }
                };
                let borders = CellBorders {
                    top: side(cb.and_then(|b| b.top.as_ref()), 0, 4, first_r),
                    right: side(cb.and_then(|b| b.right.as_ref()), 3, 5, last_c),
                    bottom: side(cb.and_then(|b| b.bottom.as_ref()), 2, 4, last_r),
                    left: side(cb.and_then(|b| b.left.as_ref()), 1, 5, first_c),
                };
                out_cells.push(Cell {
                    id: format!("{rid}.c{}", ci + 1),
                    span: c.grid_span(d).max(1),
                    vmerge: match c.v_merge(d) {
                        VMerge::Restart => Some("restart".into()),
                        VMerge::Continue => Some("continue".into()),
                        _ => None,
                    },
                    width,
                    shade: ec.shading.as_ref().and_then(|s| hex_color(s.fill.as_deref())),
                    valign: ec.v_align.as_deref().and_then(|v| match v {
                        "top" | "center" | "bottom" => Some(v.to_owned()),
                        "both" => Some("center".to_owned()),
                        _ => None,
                    }),
                    borders: (borders != CellBorders::default()).then_some(borders),
                    blocks,
                });
            }
            if out_cells.is_empty() {
                continue;
            }
            out_rows.push(Row {
                id: rid,
                header: tr_pr.is_some_and(|tp| child_on(d, tp, "tblHeader")),
                height: tr_pr
                    .and_then(|tp| d.find_child(tp, ns::W, "trHeight"))
                    .and_then(|h| w_attr(d, h, "val"))
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .map(|v| v as i64),
                rev,
                cells: out_cells,
            });
        }
        if out_rows.is_empty() {
            return None;
        }
        Some(Table {
            id: ta,
            section: None,
            style: tbl_pr.and_then(|p| child_val(d, p, "tblStyle")),
            caption: tbl_pr.and_then(|p| child_val(d, p, "tblCaption")).filter(|c| !c.trim().is_empty()),
            grid,
            bidi: tbl_pr.is_some_and(|p| child_on(d, p, "bidiVisual")),
            rows: out_rows,
        })
    }

    /// `[top, left, bottom, right, insideH, insideV]` of a table: its own `w:tblBorders`, then
    /// its style chain's.
    fn tbl_edges(&mut self, part: PartIdx, t: NodeId) -> TblEdges {
        if let Some(e) = self.border_cache.get(&(part, t)) {
            return e.clone();
        }
        let cx = self.cx;
        let d = cx.doc.part(part);
        let mut edges: [Option<Option<BorderLine>>; 6] = Default::default();
        let tbl_pr = d.find_child(t, ns::W, "tblPr");
        if let Some(tb) = tbl_pr.and_then(|p| d.find_child(p, ns::W, "tblBorders")) {
            read_tbl_borders(d, tb, &mut edges);
        }
        let style = tbl_pr
            .and_then(|p| child_val(d, p, "tblStyle"))
            .filter(|s| cx.sheet.style_by_id(s).is_some())
            .or_else(|| cx.sheet.default_table_style_id().map(str::to_owned));
        if let Some(style) = style
            && let Some(sp) = cx.doc.styles_part()
        {
            let sd = cx.doc.part(sp);
            for s in cx.sheet.style_chain(&style) {
                let Some(el) = d_style(sd, s.id()) else { continue };
                if let Some(tb) = sd.find_child(el, ns::W, "tblPr").and_then(|p| sd.find_child(p, ns::W, "tblBorders"))
                {
                    read_tbl_borders(sd, tb, &mut edges);
                }
            }
        }
        let out: TblEdges = edges.map(|e| e.flatten());
        self.border_cache.insert((part, t), out.clone());
        out
    }

    // --------------------------------------------------------------------------- comments

    fn comments(&mut self) -> Vec<Comment> {
        let cx = self.cx;
        let mut out = Vec::with_capacity(cx.comments.len());
        for c in &cx.comments {
            let mut blocks = self.story_blocks(c.story, false);
            if blocks.is_empty() {
                blocks.push(Block::P(Paragraph {
                    id: format!("{}-0", c.sid),
                    section: None,
                    style: cx.sheet.default_paragraph_style_id().unwrap_or("Normal").to_owned(),
                    style_name: None,
                    heading: None,
                    list: None,
                    fmt: None,
                    inlines: Vec::new(),
                    mark: None,
                    ppr_change: None,
                    control: None,
                    section_end: false,
                }));
            }
            let anchor = if c.parent.is_some() {
                None
            } else if let Some(sp) = self.spans.get(&c.wid)
                && let (Some((sa, so)), Some((ea, eo))) = (&sp.start, &sp.end)
            {
                Some(CommentAnchor {
                    start: sa.clone(),
                    end: ea.clone(),
                    text: sp.text.clone(),
                    start_offset: Some(*so),
                    end_offset: Some(*eo),
                })
            } else {
                self.points.get(&c.wid).map(|(a, o)| CommentAnchor {
                    start: a.clone(),
                    end: a.clone(),
                    text: String::new(),
                    start_offset: Some(*o),
                    end_offset: Some(*o),
                })
            };
            out.push(Comment {
                id: c.sid.clone(),
                parent: c.parent.clone(),
                author: c.author.clone(),
                initials: c.initials.clone().filter(|i| !i.is_empty()),
                date: c.date.as_deref().and_then(norm_date),
                done: c.done,
                agent: cx.is_agent(&c.author),
                anchor,
                blocks,
            });
        }
        out
    }

    // --------------------------------------------------------------------------- sections

    fn sections(&self) -> Vec<Section> {
        let cx = self.cx;
        let doc = cx.doc;
        let d = doc.part(doc.main_part());
        let mut story_of: HashMap<PartIdx, String> = HashMap::new();
        for (si, st) in doc.stories().iter().enumerate() {
            if matches!(st.kind, StoryKind::Header { .. } | StoryKind::Footer { .. }) {
                story_of.entry(st.part).or_insert_with(|| cx.story_id(si));
            }
        }
        let mut out = Vec::new();
        let mut hdr = HfRefs::default();
        let mut ftr = HfRefs::default();
        for (i, s) in doc.sections().iter().enumerate() {
            let sp = s.sect_pr;
            for (refs, list) in [(&mut hdr, &s.headers), (&mut ftr, &s.footers)] {
                for (typ, part) in list {
                    let id = story_of.get(part).cloned();
                    match typ {
                        HfType::Default => refs.default = id,
                        HfType::First => refs.first = id,
                        HfType::Even => refs.even = id,
                    }
                }
            }
            let num = |n: Option<NodeId>, k: &str| {
                n.and_then(|n| w_attr(d, n, k)).and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v as i64)
            };
            let pg_sz = d.find_child(sp, ns::W, "pgSz");
            let pg_mar = d.find_child(sp, ns::W, "pgMar");
            let orient = pg_sz.and_then(|p| w_attr(d, p, "orient")).filter(|o| o == "landscape" || o == "portrait");
            let cols = d
                .find_child(sp, ns::W, "cols")
                .and_then(|c| w_attr(d, c, "num"))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(1)
                .max(1);
            out.push(Section {
                id: format!("s{}", i + 1),
                page: Page {
                    w: num(pg_sz, "w").unwrap_or(12240),
                    h: num(pg_sz, "h").unwrap_or(15840),
                    orient,
                    margins: Margins {
                        top: num(pg_mar, "top").unwrap_or(1440),
                        right: num(pg_mar, "right").unwrap_or(1440),
                        bottom: num(pg_mar, "bottom").unwrap_or(1440),
                        left: num(pg_mar, "left").unwrap_or(1440),
                        header: num(pg_mar, "header"),
                        footer: num(pg_mar, "footer"),
                        gutter: num(pg_mar, "gutter").filter(|&g| g != 0),
                    },
                    cols,
                },
                headers: hdr.clone(),
                footers: ftr.clone(),
                title_page: child_on(d, sp, "titlePg"),
                break_type: child_val(d, sp, "type").filter(|t| {
                    matches!(t.as_str(), "nextPage" | "continuous" | "evenPage" | "oddPage" | "nextColumn")
                }),
            });
        }
        if out.is_empty() {
            out.push(Section {
                id: "s1".into(),
                page: Page {
                    w: 12240,
                    h: 15840,
                    orient: None,
                    margins: Margins {
                        top: 1440,
                        right: 1440,
                        bottom: 1440,
                        left: 1440,
                        header: None,
                        footer: None,
                        gutter: None,
                    },
                    cols: 1,
                },
                headers: HfRefs::default(),
                footers: HfRefs::default(),
                title_page: false,
                break_type: None,
            });
        }
        out
    }

    // ------------------------------------------------------------------------------- meta

    fn meta(&mut self) -> Meta {
        let cx = self.cx;
        let doc = cx.doc;
        let pf = render::package_flags(doc);
        let mut flags = Vec::new();
        if pf.macros {
            flags.push("macros".to_owned());
        }
        if pf.external_links {
            flags.push("external_links".to_owned());
        }
        if pf.embedded_fonts {
            flags.push("embedded_fonts".to_owned());
        }
        {
            let seen = cx.flags.borrow();
            for f in ["hidden_text", "tiny_text", "white_text"] {
                if seen.contains(f) {
                    flags.push(f.to_owned());
                }
            }
        }
        let fonts = self.fonts.iter().filter_map(|f| web_font(f).map(|w| (f.clone(), w.to_owned()))).collect();
        Meta {
            title: render::doc_title(cx, self.first_heading.clone()),
            lang: self.r.lang().unwrap_or_else(|| "und".into()),
            track_changes: ctx::track_revisions(doc),
            labels: sensitivity_labels(doc),
            pages_estimate: Some(render::doc_pages(cx, self.body_chars).max(1)),
            read_only: if pf.signed {
                Some("signed".into())
            } else if pf.protected {
                Some("protected".into())
            } else {
                None
            },
            flags,
            fonts,
        }
    }
}

fn d_style(sd: &Document, id: &str) -> Option<NodeId> {
    sd.child_elements(sd.root()).find(|&c| sd.is(c, ns::W, "style") && w_attr(sd, c, "styleId").as_deref() == Some(id))
}

/// Fill the unset slots of `edges` (`[top, left, bottom, right, insideH, insideV]`) from a
/// `w:tblBorders` element.
fn read_tbl_borders(d: &Document, tb: NodeId, edges: &mut [Option<Option<BorderLine>>; 6]) {
    for c in d.child_elements(tb) {
        if d.namespace(c) != Some(ns::W) {
            continue;
        }
        let i = match d.local_name(c) {
            Some("top") => 0,
            Some("left" | "start") => 1,
            Some("bottom") => 2,
            Some("right" | "end") => 3,
            Some("insideH") => 4,
            Some("insideV") => 5,
            _ => continue,
        };
        if edges[i].is_none() {
            let sz = w_attr(d, c, "sz").and_then(|v| v.trim().parse::<i32>().ok());
            edges[i] = Some(border_line(w_attr(d, c, "val").as_deref(), sz, w_attr(d, c, "color").as_deref()));
        }
    }
}

fn border_line(val: Option<&str>, sz: Option<i32>, color: Option<&str>) -> Option<BorderLine> {
    let v = val?;
    if matches!(v, "nil" | "none") {
        return None;
    }
    Some(BorderLine { style: v.to_owned(), size: sz.unwrap_or(4).max(0) as u32, color: hex_color(color) })
}

fn hex_color(c: Option<&str>) -> Option<String> {
    let c = c?;
    (c.len() == 6 && c.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("#{}", c.to_ascii_uppercase()))
}

fn vm_link(l: &Link) -> Option<VmLink> {
    match l {
        Link::Url(u) if inline::safe_url(u) => Some(VmLink { url: Some(u.clone()), bookmark: None }),
        Link::Url(_) => None,
        Link::Bookmark(b) => Some(VmLink { url: None, bookmark: Some(b.clone()) }),
    }
}

/// An RFC 3339 date-time, or `None`. A missing zone is read as UTC (Word writes local time
/// with a `Z` anyway).
pub(crate) fn norm_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let dig = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let ok = dig(0..4)
        && b[4] == b'-'
        && dig(5..7)
        && b[7] == b'-'
        && dig(8..10)
        && matches!(b[10], b'T' | b't')
        && dig(11..13)
        && b[13] == b':'
        && dig(14..16)
        && b[16] == b':'
        && dig(17..19);
    if !ok {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        rest = &frac[n..];
    }
    let zone_ok = rest.is_empty()
        || rest == "Z"
        || rest == "z"
        || (rest.len() == 6
            && matches!(rest.as_bytes()[0], b'+' | b'-')
            && rest.as_bytes()[3] == b':'
            && rest[1..3].bytes().all(|c| c.is_ascii_digit())
            && rest[4..6].bytes().all(|c| c.is_ascii_digit()));
    if !zone_ok {
        return None;
    }
    Some(if rest.is_empty() { format!("{s}Z") } else { s.to_owned() })
}

/// Metric-compatible web fonts the viewer substitutes.
fn web_font(f: &str) -> Option<&'static str> {
    Some(match f {
        "Calibri" | "Calibri Light" => "Carlito",
        "Cambria" => "Caladea",
        "Arial" | "Helvetica" => "Liberation Sans",
        "Arial Narrow" => "Liberation Sans Narrow",
        "Times New Roman" | "Times" => "Liberation Serif",
        "Courier New" | "Courier" => "Liberation Mono",
        "Georgia" => "Gelasio",
        "Segoe UI" => "Selawik",
        "Symbol" => "OpenSymbol",
        _ => return None,
    })
}

/// MIP sensitivity label names from `docProps/custom.xml` (`MSIP_Label_<guid>_Name`).
fn sensitivity_labels(doc: &WordDoc) -> Vec<String> {
    let Some(xml) = render::part_text(doc, "docProps/custom.xml") else { return Vec::new() };
    let mut out = Vec::new();
    let mut rest = xml.as_str();
    while let Some(i) = rest.find("name=\"MSIP_Label_") {
        rest = &rest[i + 6..];
        let Some(q) = rest.find('"') else { break };
        let name = &rest[..q];
        rest = &rest[q..];
        if !name.ends_with("_Name") {
            continue;
        }
        let Some(end) = rest.find("</property>") else { break };
        let body = &rest[..end];
        let val = body.find('>').and_then(|g| {
            let inner = &body[g + 1..];
            let s = inner.find('>')? + 1;
            let e = inner[s..].find('<')?;
            Some(inner[s..s + e].trim().to_owned())
        });
        if let Some(v) = val.filter(|v| !v.is_empty())
            && !out.contains(&v)
        {
            out.push(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(norm_date("2024-01-02T03:04:05Z").as_deref(), Some("2024-01-02T03:04:05Z"));
        assert_eq!(norm_date("2024-01-02T03:04:05").as_deref(), Some("2024-01-02T03:04:05Z"));
        assert_eq!(norm_date("2024-01-02T03:04:05.123+01:00").as_deref(), Some("2024-01-02T03:04:05.123+01:00"));
        assert_eq!(norm_date("yesterday"), None);
        assert_eq!(norm_date("2024-01-02T03:04:05Q"), None);
    }

    #[test]
    fn story_keys() {
        assert!(valid_story_key("h1"));
        assert!(valid_story_key("fn12"));
        assert!(!valid_story_key("c1"));
        assert!(!valid_story_key("h0"));
        assert!(!valid_story_key("b"));
    }
}
