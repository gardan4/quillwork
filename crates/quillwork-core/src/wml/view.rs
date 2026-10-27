//! Typed views over the XML tree (DESIGN §4.1). A view is a node id; its methods take the
//! `xml::Document` the node lives in (`WordDoc::part(story.part)`). Views never own data and
//! never go stale silently: a detached node simply yields empty results.

use super::text::{self, Segment, TextMode};
use super::tree::{Step, child_val, is_ignored_branch, is_props, is_w, parse_hex_id, parse_int, w_attr, w_local, walk};
use crate::xml::{Document, NodeId, ns};

// ---------------------------------------------------------------------- blocks

/// A block-level item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    /// A block-level content control; its blocks are [`Sdt::blocks`].
    Sdt(Sdt),
}

impl Block {
    pub fn node(self) -> NodeId {
        match self {
            Block::Paragraph(p) => p.0,
            Block::Table(t) => t.0,
            Block::Sdt(s) => s.0,
        }
    }
}

/// Direct blocks of a container (`w:body`, `w:tc`, `w:sdtContent`, `w:hdr`, `w:footnote`,
/// `w:txbxContent`, …) in order. `w:customXml`, the canonical `mc:Choice` of a block-level
/// `mc:AlternateContent` and block-level revision wrappers are looked through.
pub fn blocks(doc: &Document, container: NodeId) -> Vec<Block> {
    let mut out = Vec::new();
    direct_blocks(doc, container, &mut out);
    out
}

fn direct_blocks(doc: &Document, container: NodeId, out: &mut Vec<Block>) {
    for c in doc.child_elements(container) {
        if doc.is(c, ns::MC, "AlternateContent") {
            if let Some(ch) = doc.find_child(c, ns::MC, "Choice").or_else(|| doc.find_child(c, ns::MC, "Fallback")) {
                direct_blocks(doc, ch, out);
            }
            continue;
        }
        match w_local(doc, c) {
            Some("p") => out.push(Block::Paragraph(Paragraph(c))),
            Some("tbl") => out.push(Block::Table(Table(c))),
            Some("sdt") => out.push(Block::Sdt(Sdt(c))),
            Some("customXml" | "ins" | "del" | "moveFrom" | "moveTo") => direct_blocks(doc, c, out),
            _ => {}
        }
    }
}

/// Every paragraph and table of a story in document order (pre-order: a table comes before
/// its cells' paragraphs), descending into tables, block content controls, custom XML and
/// nested tables. Text boxes are separate stories and are not entered.
pub fn deep_blocks(doc: &Document, root: NodeId) -> Vec<Block> {
    let mut out = Vec::new();
    walk(doc, root, |n| {
        if is_ignored_branch(doc, n) {
            return Step::Over;
        }
        match w_local(doc, n) {
            Some("p") => {
                out.push(Block::Paragraph(Paragraph(n)));
                Step::Over
            }
            Some("tbl") => {
                out.push(Block::Table(Table(n)));
                Step::Into
            }
            Some("txbxContent" | "drawing" | "pict" | "object") => Step::Over,
            Some(l) if is_props(l) => Step::Over,
            _ => Step::Into,
        }
    });
    out
}

/// Every paragraph of a story in document order (see [`deep_blocks`]).
pub fn paragraphs(doc: &Document, root: NodeId) -> Vec<Paragraph> {
    deep_blocks(doc, root)
        .into_iter()
        .filter_map(|b| if let Block::Paragraph(p) = b { Some(p) } else { None })
        .collect()
}

/// The story's paragraphs as read in `mode`: like [`paragraphs`] mapped to their text, except
/// that a paragraph whose mark is a tracked deletion (accept-all) or insertion (reject-all)
/// does not end there and runs on into the next one. In [`TextMode::AsIs`] every paragraph
/// stands alone.
pub fn paragraph_texts(doc: &Document, root: NodeId, mode: TextMode) -> Vec<String> {
    let mut out = Vec::new();
    let mut carry: Option<String> = None;
    for p in paragraphs(doc, root) {
        let mut t = carry.take().unwrap_or_default();
        t.push_str(&p.text(doc, mode));
        let joins = matches!(
            (mode, p.mark_revision(doc).map(|r| r.kind)),
            (TextMode::AcceptAll, Some(RevisionKind::ParaMarkDel))
                | (TextMode::RejectAll, Some(RevisionKind::ParaMarkIns))
        );
        if joins {
            carry = Some(t);
        } else {
            out.push(t);
        }
    }
    if let Some(t) = carry {
        out.push(t);
    }
    out
}

/// Every table of a story in document order, nested tables included.
pub fn tables(doc: &Document, root: NodeId) -> Vec<Table> {
    deep_blocks(doc, root).into_iter().filter_map(|b| if let Block::Table(t) = b { Some(t) } else { None }).collect()
}

/// Walk the content of a story (not text boxes, not ignored `mc` branches) and collect the
/// elements `pick` accepts. `pick` returns `(take, descend)`.
fn collect<T>(doc: &Document, root: NodeId, mut pick: impl FnMut(NodeId) -> (Option<T>, bool)) -> Vec<T> {
    let mut out = Vec::new();
    walk(doc, root, |n| {
        if is_ignored_branch(doc, n) || is_w(doc, n, "txbxContent") {
            return Step::Over;
        }
        let (take, descend) = pick(n);
        if let Some(t) = take {
            out.push(t);
        }
        if descend { Step::Into } else { Step::Over }
    });
    out
}

// ---------------------------------------------------------------------- paragraph & run

/// `w:numPr` contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumPr {
    pub num_id: Option<i64>,
    pub ilvl: Option<i64>,
}

pub(crate) fn read_num_pr(doc: &Document, ppr: NodeId) -> Option<NumPr> {
    let np = doc.find_child(ppr, ns::W, "numPr")?;
    Some(NumPr {
        num_id: child_val(doc, np, "numId").and_then(|v| parse_int(&v)),
        ilvl: child_val(doc, np, "ilvl").and_then(|v| parse_int(&v)),
    })
}

/// A `w:p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Paragraph(pub NodeId);

impl Paragraph {
    pub fn node(self) -> NodeId {
        self.0
    }

    /// `w14:paraId`.
    pub fn para_id(self, doc: &Document) -> Option<u32> {
        doc.attr_ns(self.0, ns::W14, "paraId").and_then(|v| parse_hex_id(&v))
    }

    /// `w14:textId`.
    pub fn text_id(self, doc: &Document) -> Option<u32> {
        doc.attr_ns(self.0, ns::W14, "textId").and_then(|v| parse_hex_id(&v))
    }

    pub fn ppr(self, doc: &Document) -> Option<NodeId> {
        doc.find_child(self.0, ns::W, "pPr")
    }

    /// `w:pStyle/@w:val`.
    pub fn style_id(self, doc: &Document) -> Option<String> {
        child_val(doc, self.ppr(doc)?, "pStyle")
    }

    /// Direct `w:numPr` (not resolved through styles; see `numbering`).
    pub fn num_pr(self, doc: &Document) -> Option<NumPr> {
        read_num_pr(doc, self.ppr(doc)?)
    }

    pub fn text(self, doc: &Document, mode: TextMode) -> String {
        text::text(doc, self.0, mode)
    }

    /// As-is text split by revision state.
    pub fn segments(self, doc: &Document) -> Vec<Segment> {
        text::segments(doc, self.0)
    }

    /// Is the paragraph mark a tracked insertion / deletion (`w:pPr/w:rPr/w:ins|w:del`)?
    pub fn mark_revision(self, doc: &Document) -> Option<Revision> {
        let rpr = doc.find_child(self.ppr(doc)?, ns::W, "rPr")?;
        doc.child_elements(rpr).find_map(|c| Revision::classify(doc, c))
    }

    /// Runs of the paragraph in order, through hyperlinks, fields, content controls, smart
    /// tags and revision wrappers (deleted runs included).
    pub fn runs(self, doc: &Document) -> Vec<Run> {
        collect(doc, self.0, |n| match w_local(doc, n) {
            Some("r") => (Some(Run(n)), false),
            Some("drawing" | "pict" | "object") => (None, false),
            Some(l) if is_props(l) => (None, false),
            _ => (None, true),
        })
    }

    /// The paragraph's containing table cell, if any.
    pub fn cell(self, doc: &Document) -> Option<Cell> {
        doc.ancestors(self.0).take_while(|&a| !is_w(doc, a, "txbxContent")).find(|&a| is_w(doc, a, "tc")).map(Cell)
    }
}

/// A `w:r`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Run(pub NodeId);

impl Run {
    pub fn node(self) -> NodeId {
        self.0
    }
    pub fn rpr(self, doc: &Document) -> Option<NodeId> {
        doc.find_child(self.0, ns::W, "rPr")
    }
    /// `w:rStyle/@w:val`.
    pub fn style_id(self, doc: &Document) -> Option<String> {
        child_val(doc, self.rpr(doc)?, "rStyle")
    }
    pub fn text(self, doc: &Document, mode: TextMode) -> String {
        if let Some(rev) = self.revision(doc) {
            let hidden = match mode {
                TextMode::AsIs => false,
                TextMode::AcceptAll => matches!(rev.kind, RevisionKind::Del | RevisionKind::MoveFrom),
                TextMode::RejectAll => matches!(rev.kind, RevisionKind::Ins | RevisionKind::MoveTo),
            };
            if hidden {
                return String::new();
            }
        }
        text::text(doc, self.0, mode)
    }
    /// Nearest enclosing `w:ins`/`w:del`/`w:moveFrom`/`w:moveTo` inside the paragraph.
    pub fn revision(self, doc: &Document) -> Option<Revision> {
        doc.ancestors(self.0)
            .take_while(|&a| !is_w(doc, a, "p"))
            .find_map(|a| Revision::classify(doc, a).filter(|r| r.kind.is_content()))
    }
    /// `w:rPr/w:rPrChange`.
    pub fn property_change(self, doc: &Document) -> Option<Revision> {
        doc.find_child(self.rpr(doc)?, ns::W, "rPrChange").and_then(|c| Revision::classify(doc, c))
    }
    /// `w:fldChar` in this run, as (node, `@w:fldCharType`).
    pub fn fld_char(self, doc: &Document) -> Option<(NodeId, String)> {
        let f = doc.find_child(self.0, ns::W, "fldChar")?;
        Some((f, w_attr(doc, f, "fldCharType").unwrap_or_default()))
    }
}

// ---------------------------------------------------------------------- tables

/// A `w:tbl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Table(pub NodeId);

/// Children of `n` named `w:<local>`, looking through `w:sdt/w:sdtContent`, `w:customXml` and
/// revision wrappers (row- and cell-level content controls).
fn wrapped_children(doc: &Document, n: NodeId, local: &str, out: &mut Vec<NodeId>) {
    for c in doc.child_elements(n) {
        match w_local(doc, c) {
            Some(l) if l == local => out.push(c),
            Some("sdt") => {
                if let Some(content) = doc.find_child(c, ns::W, "sdtContent") {
                    wrapped_children(doc, content, local, out);
                }
            }
            Some("customXml" | "ins" | "del" | "moveFrom" | "moveTo") => wrapped_children(doc, c, local, out),
            _ => {}
        }
    }
}

impl Table {
    pub fn node(self) -> NodeId {
        self.0
    }
    pub fn rows(self, doc: &Document) -> Vec<Row> {
        let mut v = Vec::new();
        wrapped_children(doc, self.0, "tr", &mut v);
        v.into_iter().map(Row).collect()
    }
    /// `w:tblPr/w:tblStyle/@w:val`.
    pub fn style_id(self, doc: &Document) -> Option<String> {
        child_val(doc, doc.find_child(self.0, ns::W, "tblPr")?, "tblStyle")
    }
    /// First paragraph of the first cell (what a table is identified by).
    pub fn first_paragraph(self, doc: &Document) -> Option<Paragraph> {
        paragraphs(doc, self.0).into_iter().next()
    }
}

/// A `w:tr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Row(pub NodeId);

impl Row {
    pub fn node(self) -> NodeId {
        self.0
    }
    pub fn cells(self, doc: &Document) -> Vec<Cell> {
        let mut v = Vec::new();
        wrapped_children(doc, self.0, "tc", &mut v);
        v.into_iter().map(Cell).collect()
    }
    /// `w:trPr/w:ins|w:del`.
    pub fn revision(self, doc: &Document) -> Option<Revision> {
        let trpr = doc.find_child(self.0, ns::W, "trPr")?;
        doc.child_elements(trpr).find_map(|c| Revision::classify(doc, c))
    }
}

/// Vertical merge state of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VMerge {
    None,
    Restart,
    Continue,
}

/// A `w:tc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cell(pub NodeId);

impl Cell {
    pub fn node(self) -> NodeId {
        self.0
    }
    pub fn blocks(self, doc: &Document) -> Vec<Block> {
        blocks(doc, self.0)
    }
    pub fn paragraphs(self, doc: &Document) -> Vec<Paragraph> {
        paragraphs(doc, self.0)
    }
    fn tcpr(self, doc: &Document) -> Option<NodeId> {
        doc.find_child(self.0, ns::W, "tcPr")
    }
    /// `w:gridSpan/@w:val` (1 when absent).
    pub fn grid_span(self, doc: &Document) -> u32 {
        self.tcpr(doc).and_then(|p| child_val(doc, p, "gridSpan")).and_then(|v| v.trim().parse().ok()).unwrap_or(1)
    }
    pub fn v_merge(self, doc: &Document) -> VMerge {
        let Some(vm) = self.tcpr(doc).and_then(|p| doc.find_child(p, ns::W, "vMerge")) else { return VMerge::None };
        match w_attr(doc, vm, "val").as_deref() {
            Some("restart") => VMerge::Restart,
            _ => VMerge::Continue,
        }
    }
    pub fn text(self, doc: &Document, mode: TextMode) -> String {
        text::text(doc, self.0, mode)
    }
}

// ---------------------------------------------------------------------- inline things

/// A `w:hyperlink`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hyperlink(pub NodeId);

impl Hyperlink {
    /// `r:id` (external target via the part's relationships).
    pub fn rel_id(self, doc: &Document) -> Option<String> {
        doc.attr_ns(self.0, ns::R, "id").map(|c| c.into_owned())
    }
    /// `w:anchor` (a bookmark in this document).
    pub fn anchor(self, doc: &Document) -> Option<String> {
        w_attr(doc, self.0, "anchor")
    }
    pub fn text(self, doc: &Document, mode: TextMode) -> String {
        text::text(doc, self.0, mode)
    }
}

/// Every `w:hyperlink` of a story.
pub fn hyperlinks(doc: &Document, root: NodeId) -> Vec<Hyperlink> {
    collect(doc, root, |n| if is_w(doc, n, "hyperlink") { (Some(Hyperlink(n)), true) } else { (None, true) })
}

/// What kind of graphic element a [`Drawing`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrawingKind {
    /// `w:drawing` (DrawingML).
    Drawing,
    /// `w:pict` (VML).
    Pict,
    /// `w:object` (OLE).
    Object,
}

/// A `w:drawing`, `w:pict` or `w:object`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Drawing(pub NodeId);

impl Drawing {
    pub fn kind(self, doc: &Document) -> Option<DrawingKind> {
        match w_local(doc, self.0)? {
            "drawing" => Some(DrawingKind::Drawing),
            "pict" => Some(DrawingKind::Pict),
            "object" => Some(DrawingKind::Object),
            _ => None,
        }
    }
    fn doc_pr(self, doc: &Document) -> Option<NodeId> {
        doc.descendants(self.0).find(|&n| doc.is(n, ns::WP, "docPr"))
    }
    /// `wp:docPr/@id`.
    pub fn doc_pr_id(self, doc: &Document) -> Option<u32> {
        doc.attr(self.doc_pr(doc)?, "id")?.trim().parse().ok()
    }
    /// `wp:docPr/@name`.
    pub fn name(self, doc: &Document) -> Option<String> {
        doc.attr(self.doc_pr(doc)?, "name").map(|c| c.into_owned())
    }
    /// `wp:docPr/@descr` (alt text).
    pub fn description(self, doc: &Document) -> Option<String> {
        doc.attr(self.doc_pr(doc)?, "descr").map(|c| c.into_owned())
    }
    /// Relationship ids of embedded/linked media (`r:embed`, `r:link`, `r:id`) in the drawing.
    pub fn rel_ids(self, doc: &Document) -> Vec<String> {
        let mut out = Vec::new();
        for n in doc.descendants(self.0) {
            if !doc.is_element_node(n) || doc.attr_count(n) == 0 {
                continue;
            }
            for local in ["embed", "link", "id"] {
                if let Some(v) = doc.attr_ns(n, ns::R, local)
                    && !out.iter().any(|o: &String| *o == *v)
                {
                    out.push(v.into_owned());
                }
            }
        }
        out
    }
    /// The paragraph holding the drawing.
    pub fn paragraph(self, doc: &Document) -> Option<Paragraph> {
        doc.ancestors(self.0).find(|&a| is_w(doc, a, "p")).map(Paragraph)
    }
}

/// Every drawing of a story (`w:drawing`, `w:pict`, `w:object`), not descending into them.
pub fn drawings(doc: &Document, root: NodeId) -> Vec<Drawing> {
    collect(doc, root, |n| match w_local(doc, n) {
        Some("drawing" | "pict" | "object") => (Some(Drawing(n)), false),
        Some(l) if is_props(l) => (None, false),
        _ => (None, true),
    })
}

/// A `w:sdt` (block, row, cell or run level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sdt(pub NodeId);

impl Sdt {
    pub fn node(self) -> NodeId {
        self.0
    }
    fn pr(self, doc: &Document) -> Option<NodeId> {
        doc.find_child(self.0, ns::W, "sdtPr")
    }
    pub fn content(self, doc: &Document) -> Option<NodeId> {
        doc.find_child(self.0, ns::W, "sdtContent")
    }
    pub fn tag(self, doc: &Document) -> Option<String> {
        child_val(doc, self.pr(doc)?, "tag")
    }
    pub fn alias(self, doc: &Document) -> Option<String> {
        child_val(doc, self.pr(doc)?, "alias")
    }
    pub fn id(self, doc: &Document) -> Option<i64> {
        child_val(doc, self.pr(doc)?, "id").and_then(|v| parse_int(&v))
    }
    /// Is this a content control whose content Word locks (`w:lock` contentLocked/sdtContentLocked)?
    pub fn content_locked(self, doc: &Document) -> bool {
        self.pr(doc)
            .and_then(|p| child_val(doc, p, "lock"))
            .is_some_and(|v| v == "contentLocked" || v == "sdtContentLocked")
    }
    /// Block-level content (empty for run-level controls).
    pub fn blocks(self, doc: &Document) -> Vec<Block> {
        self.content(doc).map(|c| blocks(doc, c)).unwrap_or_default()
    }
}

/// Every `w:sdt` of a story.
pub fn sdts(doc: &Document, root: NodeId) -> Vec<Sdt> {
    collect(doc, root, |n| if is_w(doc, n, "sdt") { (Some(Sdt(n)), true) } else { (None, true) })
}

/// A bookmark: `w:bookmarkStart` plus its matching `w:bookmarkEnd` in the same story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bookmark {
    pub start: NodeId,
    pub end: Option<NodeId>,
}

impl Bookmark {
    pub fn name(self, doc: &Document) -> Option<String> {
        w_attr(doc, self.start, "name")
    }
    pub fn id(self, doc: &Document) -> Option<String> {
        w_attr(doc, self.start, "id")
    }
}

/// Every bookmark of a story, start order.
pub fn bookmarks(doc: &Document, root: NodeId) -> Vec<Bookmark> {
    let mut starts: Vec<(String, NodeId)> = Vec::new();
    let mut ends: Vec<(String, NodeId)> = Vec::new();
    for n in doc.descendants(root) {
        if is_w(doc, n, "bookmarkStart") {
            starts.push((w_attr(doc, n, "id").unwrap_or_default(), n));
        } else if is_w(doc, n, "bookmarkEnd") {
            ends.push((w_attr(doc, n, "id").unwrap_or_default(), n));
        }
    }
    starts
        .into_iter()
        .map(|(id, s)| Bookmark { start: s, end: ends.iter().find(|(e, _)| *e == id).map(|(_, n)| *n) })
        .collect()
}

/// What a comment marker is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommentMarkerKind {
    RangeStart,
    RangeEnd,
    Reference,
}

/// A `w:commentRangeStart`, `w:commentRangeEnd` or `w:commentReference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommentMarker {
    pub node: NodeId,
    pub kind: CommentMarkerKind,
}

impl CommentMarker {
    /// `@w:id` (matches `w:comment/@w:id`).
    pub fn comment_id(self, doc: &Document) -> Option<i64> {
        w_attr(doc, self.node, "id").and_then(|v| parse_int(&v))
    }
}

/// Every comment marker of a story, in document order.
pub fn comment_markers(doc: &Document, root: NodeId) -> Vec<CommentMarker> {
    collect(doc, root, |n| {
        let kind = match w_local(doc, n) {
            Some("commentRangeStart") => CommentMarkerKind::RangeStart,
            Some("commentRangeEnd") => CommentMarkerKind::RangeEnd,
            Some("commentReference") => CommentMarkerKind::Reference,
            _ => return (None, true),
        };
        (Some(CommentMarker { node: n, kind }), false)
    })
}

// ---------------------------------------------------------------------- revisions

/// What a tracked change does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevisionKind {
    /// `w:ins` around runs.
    Ins,
    /// `w:del` around runs.
    Del,
    MoveFrom,
    MoveTo,
    /// `w:pPr/w:rPr/w:ins`: the paragraph mark was inserted (a paragraph split / new paragraph).
    ParaMarkIns,
    /// `w:pPr/w:rPr/w:del`: the paragraph mark was deleted (paragraphs merged).
    ParaMarkDel,
    /// `w:trPr/w:ins` / `w:trPr/w:del`.
    RowIns,
    RowDel,
    CellIns,
    CellDel,
    CellMerge,
    /// `w:rPrChange`.
    RunProps,
    /// `w:pPrChange`.
    ParaProps,
    SectProps,
    TableProps,
    TableExceptionProps,
    RowProps,
    CellProps,
    Numbering,
}

impl RevisionKind {
    /// Changes content (text in/out) rather than formatting or structure.
    pub fn is_content(self) -> bool {
        matches!(self, RevisionKind::Ins | RevisionKind::Del | RevisionKind::MoveFrom | RevisionKind::MoveTo)
    }
    /// A formatting (property) change.
    pub fn is_property(self) -> bool {
        matches!(
            self,
            RevisionKind::RunProps
                | RevisionKind::ParaProps
                | RevisionKind::SectProps
                | RevisionKind::TableProps
                | RevisionKind::TableExceptionProps
                | RevisionKind::RowProps
                | RevisionKind::CellProps
                | RevisionKind::Numbering
        )
    }
}

/// A tracked change element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Revision {
    pub node: NodeId,
    pub kind: RevisionKind,
}

impl Revision {
    /// Classify `n` as a revision element, if it is one.
    pub fn classify(doc: &Document, n: NodeId) -> Option<Revision> {
        let local = w_local(doc, n)?;
        let parent = doc.parent(n).and_then(|p| w_local(doc, p).map(|l| (p, l)));
        let in_mark = |doc: &Document| {
            parent.is_some_and(|(p, l)| l == "rPr" && doc.parent(p).is_some_and(|pp| is_w(doc, pp, "pPr")))
        };
        let kind = match local {
            "ins" | "del" => {
                let ins = local == "ins";
                match parent.map(|(_, l)| l) {
                    Some("trPr") => {
                        if ins {
                            RevisionKind::RowIns
                        } else {
                            RevisionKind::RowDel
                        }
                    }
                    Some("rPr") if in_mark(doc) => {
                        if ins {
                            RevisionKind::ParaMarkIns
                        } else {
                            RevisionKind::ParaMarkDel
                        }
                    }
                    Some("rPr" | "numPr") => return None, // run-mark / numbering-level markers: not counted
                    _ => {
                        if ins {
                            RevisionKind::Ins
                        } else {
                            RevisionKind::Del
                        }
                    }
                }
            }
            "moveFrom" => RevisionKind::MoveFrom,
            "moveTo" => RevisionKind::MoveTo,
            "cellIns" => RevisionKind::CellIns,
            "cellDel" => RevisionKind::CellDel,
            "cellMerge" => RevisionKind::CellMerge,
            "rPrChange" => RevisionKind::RunProps,
            "pPrChange" => RevisionKind::ParaProps,
            "sectPrChange" => RevisionKind::SectProps,
            "tblPrChange" => RevisionKind::TableProps,
            "tblPrExChange" => RevisionKind::TableExceptionProps,
            "trPrChange" => RevisionKind::RowProps,
            "tcPrChange" => RevisionKind::CellProps,
            "numberingChange" => RevisionKind::Numbering,
            _ => return None,
        };
        Some(Revision { node: n, kind })
    }

    /// `@w:id`.
    pub fn id(self, doc: &Document) -> Option<i64> {
        w_attr(doc, self.node, "id").and_then(|v| parse_int(&v))
    }
    pub fn author(self, doc: &Document) -> Option<String> {
        w_attr(doc, self.node, "author")
    }
    pub fn date(self, doc: &Document) -> Option<String> {
        w_attr(doc, self.node, "date")
    }
    /// As-is text inside a content revision.
    pub fn text(self, doc: &Document) -> String {
        text::text(doc, self.node, TextMode::AsIs)
    }
}

/// Every revision of a story in document order (property changes included; revisions inside
/// another revision are listed too).
pub fn revisions(doc: &Document, root: NodeId) -> Vec<Revision> {
    let mut out = Vec::new();
    walk(doc, root, |n| {
        if is_ignored_branch(doc, n) || is_w(doc, n, "txbxContent") {
            return Step::Over;
        }
        if let Some(r) = Revision::classify(doc, n) {
            out.push(r);
            // A property change holds the *old* properties: nothing inside is a live revision.
            return if r.kind.is_property() { Step::Over } else { Step::Into };
        }
        Step::Into
    });
    out
}
